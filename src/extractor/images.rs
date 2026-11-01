//! Docmost addition: embedded image extraction.
//!
//! Upstream's extractor emits an `ItemType::Image` item for every Image
//! XObject, on the page and inside Form XObjects, but keeps no pixels. This
//! module decodes those items' XObjects and gives each decoded item a
//! `pdf-image://N` text that the markdown generator renders as a placeholder.

use super::extract_positioned_text_from_doc;
use crate::tounicode::FontCMaps;
use crate::types::{ItemType, TextItem};
use crate::PdfError;
use lopdf::{Document, Object, ObjectId};
use std::collections::BTreeMap;

/// Scheme of the markdown placeholders: `pdf-image://N` is `images[N]`.
pub(crate) const PDF_IMAGE_SCHEME: &str = "pdf-image://";

/// Image format for extracted images.
#[derive(Debug, Clone)]
pub enum ImageFormat {
    /// JPEG image (DCTDecode filter) — raw stream content is a valid JPEG file.
    Jpeg,
    /// PNG image (FlateDecode filter) — raw pixels encoded to PNG.
    Png,
    // TODO: Jpeg2000 (JPXDecode), Tiff (CCITTFaxDecode)
}

/// An image extracted from a PDF.
#[derive(Debug, Clone)]
pub struct ExtractedImage {
    /// Page number (1-indexed).
    pub page: u32,
    /// X position on page.
    pub x: f32,
    /// Y position on page.
    pub y: f32,
    /// Pixel width from PDF image dictionary.
    pub width: u32,
    /// Pixel height from PDF image dictionary.
    pub height: u32,
    /// Image format.
    pub format: ImageFormat,
    /// Raw image bytes.
    pub data: Vec<u8>,
}

/// Extract images from PDF memory buffer.
///
/// Extracts JPEG (DCTDecode) and PNG (FlateDecode) images.
/// Returns raw image bytes with position and dimension metadata.
pub fn extract_images_mem(buffer: &[u8]) -> Result<Vec<ExtractedImage>, PdfError> {
    crate::validate_pdf_bytes(buffer)?;
    let (doc, _) = crate::load_document_from_mem(buffer)?;
    let font_cmaps = FontCMaps::from_doc(&doc);
    let ((mut items, _, _), ..) = extract_positioned_text_from_doc(&doc, &font_cmaps, None)?;
    Ok(collect_images(&doc, &mut items))
}

/// Whether a document classified as a scan carries an invisible text layer.
/// Upstream stops there and leaves such a scan to the caller's OCR. Docmost has
/// no OCR step, so the page rasters are the only copy of those pages it can keep.
pub(crate) fn has_invisible_text_layer(ocr_reasons_by_page: &BTreeMap<u32, Vec<String>>) -> bool {
    ocr_reasons_by_page
        .values()
        .flatten()
        .any(|reason| reason == crate::OCR_REASON_INVISIBLE_TEXT_LAYER)
}

/// What an Image item keeps in `font_tag`: the XObject it paints. The resource
/// name in its text is not enough, each Form XObject has its own namespace.
pub(crate) fn image_object_tag(id: ObjectId) -> String {
    format!("{} {}", id.0, id.1)
}

fn parse_image_object_tag(tag: &str) -> Option<ObjectId> {
    let (number, generation) = tag.split_once(' ')?;
    Some((number.parse().ok()?, generation.parse().ok()?))
}

/// Decode the XObject of every Image item and point the item at its result
/// with a `pdf-image://N` text. Items whose format is not supported keep their
/// text: the markdown generator still needs their regions for line grouping,
/// but renders no placeholder for them.
pub(crate) fn collect_images(doc: &Document, items: &mut [TextItem]) -> Vec<ExtractedImage> {
    let mut images = Vec::new();
    for item in items
        .iter_mut()
        .filter(|item| matches!(item.item_type, ItemType::Image))
    {
        let image = parse_image_object_tag(&item.font_tag)
            .and_then(|id| extract_image_xobject(doc, id, item.page, item.x, item.y));
        if let Some(image) = image {
            item.text = format!("{}{}", PDF_IMAGE_SCHEME, images.len());
            images.push(image);
        }
    }
    images
}

/// Extract an image from an XObject stream, returning an `ExtractedImage`
/// if the filter is supported (DCTDecode / FlateDecode).
fn extract_image_xobject(
    doc: &Document,
    obj_id: ObjectId,
    page_num: u32,
    x: f32,
    y: f32,
) -> Option<ExtractedImage> {
    let stream = match doc.get_object(obj_id) {
        Ok(Object::Stream(s)) => s,
        _ => return None,
    };

    // Filter can be a name (/FlateDecode) or array ([/FlateDecode])
    let filter = stream.dict.get(b"Filter").ok().and_then(|f| {
        if let Ok(name) = f.as_name() {
            Some(name.to_vec())
        } else if let Ok(arr) = f.as_array() {
            if arr.len() == 1 {
                arr[0].as_name().ok().map(|n| n.to_vec())
            } else {
                // Chained filters like [/ASCII85Decode /FlateDecode]
                arr.last()
                    .and_then(|v| v.as_name().ok())
                    .map(|n| n.to_vec())
            }
        } else {
            None
        }
    });

    let width = stream
        .dict
        .get(b"Width")
        .ok()
        .and_then(|w| w.as_i64().ok())
        .unwrap_or(0) as u32;
    let height = stream
        .dict
        .get(b"Height")
        .ok()
        .and_then(|h| h.as_i64().ok())
        .unwrap_or(0) as u32;

    if width == 0 || height == 0 {
        return None;
    }

    match filter.as_deref() {
        Some(b"DCTDecode") => {
            // For chained filters like [/FlateDecode /DCTDecode], the JPEG
            // payload is wrapped in additional compression layers. Apply each
            // filter in array order; DCTDecode marks the end (the result IS
            // the JPEG file).
            let data = decode_filters_for_jpeg(stream)?;
            // Sanity check: a real JPEG must start with the SOI marker FFD8.
            // If it doesn't, something is wrong (unsupported filter chain or
            // mis-detected DCT) — skip rather than emit a corrupt file.
            if data.len() < 2 || data[0] != 0xFF || data[1] != 0xD8 {
                return None;
            }
            Some(ExtractedImage {
                page: page_num,
                x,
                y,
                width,
                height,
                format: ImageFormat::Jpeg,
                data,
            })
        }
        Some(b"FlateDecode") => encode_flatedecode_to_png(stream, width, height, page_num, x, y),
        _ => None,
    }
}

/// Apply the filter chain on a DCTDecode stream and return the raw JPEG bytes.
///
/// Handles common pre-DCT filters (FlateDecode, ASCII85Decode, ASCIIHexDecode).
/// Returns `None` if any unsupported filter appears.
fn decode_filters_for_jpeg(stream: &lopdf::Stream) -> Option<Vec<u8>> {
    let filters: Vec<Vec<u8>> = match stream.dict.get(b"Filter").ok()? {
        Object::Name(n) => vec![n.clone()],
        Object::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_name().ok().map(|n| n.to_vec()))
            .collect(),
        _ => return None,
    };

    let mut data = stream.content.clone();
    for filter in &filters {
        match filter.as_slice() {
            // End of the chain — the bytes ARE the JPEG file.
            b"DCTDecode" => return Some(data),
            b"FlateDecode" => {
                use flate2::read::ZlibDecoder;
                use std::io::Read;
                let mut decoder = ZlibDecoder::new(&data[..]);
                let mut out = Vec::new();
                if decoder.read_to_end(&mut out).is_err() {
                    return None;
                }
                data = out;
            }
            b"ASCIIHexDecode" => {
                data = ascii_hex_decode(&data)?;
            }
            b"ASCII85Decode" => {
                data = ascii85_decode(&data)?;
            }
            // Other filters (LZWDecode, RunLengthDecode, JBIG2Decode) — skip.
            _ => return None,
        }
    }
    // Reached end of chain without a DCTDecode marker — shouldn't happen
    // since we only call this for DCT-tagged streams.
    Some(data)
}

/// Decode an ASCIIHex-encoded byte stream. Whitespace is ignored, `>` ends.
fn ascii_hex_decode(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut nibble: Option<u8> = None;
    for &b in data {
        if b == b'>' {
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => return None,
        };
        match nibble {
            Some(hi) => {
                out.push((hi << 4) | v);
                nibble = None;
            }
            None => nibble = Some(v),
        }
    }
    // Trailing odd nibble: low half is implicitly 0.
    if let Some(hi) = nibble {
        out.push(hi << 4);
    }
    Some(out)
}

/// Decode an ASCII85-encoded byte stream. `~>` ends.
fn ascii85_decode(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 4 / 5);
    let mut group: [u8; 5] = [0; 5];
    let mut count: usize = 0;
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        i += 1;
        if b == b'~' {
            // EOD marker `~>`
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'z' && count == 0 {
            // Shorthand for four zero bytes.
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&b) {
            return None;
        }
        group[count] = b - b'!';
        count += 1;
        if count == 5 {
            let value = group.iter().try_fold(0u32, |acc, &c| {
                acc.checked_mul(85).and_then(|v| v.checked_add(c as u32))
            })?;
            out.push((value >> 24) as u8);
            out.push((value >> 16) as u8);
            out.push((value >> 8) as u8);
            out.push(value as u8);
            count = 0;
        }
    }
    if count > 0 {
        // Pad with `u` (84) and emit count-1 bytes.
        for slot in group.iter_mut().skip(count) {
            *slot = 84;
        }
        let value = group.iter().try_fold(0u32, |acc, &c| {
            acc.checked_mul(85).and_then(|v| v.checked_add(c as u32))
        })?;
        let bytes = [
            (value >> 24) as u8,
            (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
        ];
        out.extend_from_slice(&bytes[..count - 1]);
    }
    Some(out)
}

/// Color space info resolved from a PDF image dictionary.
struct ImageColorInfo {
    png_color_type: png::ColorType,
    channels: u8,
    is_cmyk: bool,
}

/// Decompress a FlateDecode image stream and encode it as PNG.
///
/// Handles DeviceRGB, DeviceGray, DeviceCMYK color spaces.
/// Handles PNG predictor filters (Predictor >= 10 in DecodeParms).
/// Skips unsupported color spaces (Indexed, etc.) instead of crashing.
fn encode_flatedecode_to_png(
    stream: &lopdf::Stream,
    width: u32,
    height: u32,
    page_num: u32,
    x: f32,
    y: f32,
) -> Option<ExtractedImage> {
    // Decompress the stream
    let raw = stream.decompressed_content().ok()?;

    let bpc = stream
        .dict
        .get(b"BitsPerComponent")
        .ok()
        .and_then(|b| b.as_i64().ok())
        .unwrap_or(8) as u8;

    let color_info = resolve_color_space(&stream.dict)?;

    // Check for PNG predictor in DecodeParms
    let has_png_predictor = stream
        .dict
        .get(b"DecodeParms")
        .ok()
        .and_then(|dp| dp.as_dict().ok())
        .and_then(|dp| dp.get(b"Predictor").ok())
        .and_then(|p| p.as_i64().ok())
        .map(|p| p >= 10)
        .unwrap_or(false);

    // Calculate expected row size (bytes per row of pixel data, no filter byte)
    let bits_per_row = width as usize * color_info.channels as usize * bpc as usize;
    let bytes_per_row = bits_per_row.div_ceil(8);

    // Prepare pixel data: strip PNG predictor filter bytes if present
    let pixel_data = if has_png_predictor {
        let stride = bytes_per_row + 1; // +1 for filter byte
        if raw.len() < stride * height as usize {
            return None;
        }
        unfilter_png_rows(
            &raw,
            width,
            height,
            bytes_per_row,
            color_info.channels as usize,
            bpc,
        )
    } else {
        if raw.len() < bytes_per_row * height as usize {
            return None;
        }
        // Truncate to exact expected size to avoid PNG encoder errors
        raw[..bytes_per_row * height as usize].to_vec()
    };

    // Handle CMYK → RGB conversion
    let (final_data, final_color_type) = if color_info.is_cmyk {
        let rgb = cmyk_to_rgb(&pixel_data, width, height);
        (rgb, png::ColorType::Rgb)
    } else {
        (pixel_data, color_info.png_color_type)
    };

    // Encode to PNG
    let mut png_buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_buf, width, height);
        encoder.set_color(final_color_type);
        encoder.set_depth(match bpc {
            1 => png::BitDepth::One,
            2 => png::BitDepth::Two,
            4 => png::BitDepth::Four,
            16 => png::BitDepth::Sixteen,
            _ => png::BitDepth::Eight,
        });
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&final_data).ok()?;
    }

    Some(ExtractedImage {
        page: page_num,
        x,
        y,
        width,
        height,
        format: ImageFormat::Png,
        data: png_buf,
    })
}

/// Resolve PDF ColorSpace to PNG color type and channel count.
///
/// Returns `None` for unsupported color spaces (Indexed, patterns, etc.)
/// to skip the image rather than crash.
fn resolve_color_space(dict: &lopdf::Dictionary) -> Option<ImageColorInfo> {
    let cs = dict.get(b"ColorSpace").ok().and_then(|cs| {
        if let Ok(name) = cs.as_name() {
            Some(name.to_vec())
        } else if let Ok(arr) = cs.as_array() {
            arr.first()
                .and_then(|v| v.as_name().ok())
                .map(|n| n.to_vec())
        } else {
            None
        }
    });

    match cs.as_deref() {
        Some(b"DeviceRGB") | Some(b"CalRGB") => Some(ImageColorInfo {
            png_color_type: png::ColorType::Rgb,
            channels: 3,
            is_cmyk: false,
        }),
        Some(b"DeviceGray") | Some(b"CalGray") => Some(ImageColorInfo {
            png_color_type: png::ColorType::Grayscale,
            channels: 1,
            is_cmyk: false,
        }),
        Some(b"DeviceCMYK") => Some(ImageColorInfo {
            png_color_type: png::ColorType::Rgb, // will be converted
            channels: 4,
            is_cmyk: true,
        }),
        Some(b"ICCBased") => {
            // ICCBased: [/ICCBased stream_ref]. Infer channels from BPC + data,
            // or fall back to RGB. Safe to guess RGB(3) for most ICC profiles.
            Some(ImageColorInfo {
                png_color_type: png::ColorType::Rgb,
                channels: 3,
                is_cmyk: false,
            })
        }
        Some(b"Indexed") => {
            // Indexed requires a PLTE chunk which we don't extract.
            // Skip rather than crash.
            None
        }
        None => {
            // No ColorSpace specified — default to RGB
            Some(ImageColorInfo {
                png_color_type: png::ColorType::Rgb,
                channels: 3,
                is_cmyk: false,
            })
        }
        _ => None, // Unknown color space — skip
    }
}

/// Apply PNG un-filtering to rows with predictor filter bytes.
///
/// PDF spec §7.4.4.4: Predictor values 10–15 correspond to PNG filter types
/// (None=0, Sub=1, Up=2, Average=3, Paeth=4).
fn unfilter_png_rows(
    data: &[u8],
    _width: u32,
    height: u32,
    bytes_per_row: usize,
    channels: usize,
    bpc: u8,
) -> Vec<u8> {
    let stride = bytes_per_row + 1; // row data + filter byte
    let bpp = std::cmp::max(1, (channels * bpc as usize) / 8); // bytes per pixel
    let mut result = vec![0u8; bytes_per_row * height as usize];
    let mut prev_row = vec![0u8; bytes_per_row];

    for row in 0..height as usize {
        let src_offset = row * stride;
        if src_offset >= data.len() {
            break;
        }
        let filter_type = data[src_offset];
        let src_start = src_offset + 1;
        let src_end = std::cmp::min(src_start + bytes_per_row, data.len());
        let dst_start = row * bytes_per_row;

        let row_data = &data[src_start..src_end];
        let dst = &mut result[dst_start..dst_start + bytes_per_row];

        match filter_type {
            0 => {
                // None
                dst[..row_data.len()].copy_from_slice(row_data);
            }
            1 => {
                // Sub: each byte is added to the byte `bpp` positions to the left
                for i in 0..row_data.len() {
                    let left = if i >= bpp { dst[i - bpp] } else { 0 };
                    dst[i] = row_data[i].wrapping_add(left);
                }
            }
            2 => {
                // Up: each byte is added to the byte directly above
                for i in 0..row_data.len() {
                    dst[i] = row_data[i].wrapping_add(prev_row[i]);
                }
            }
            3 => {
                // Average: each byte uses the average of left and above
                for i in 0..row_data.len() {
                    let left = if i >= bpp { dst[i - bpp] as u16 } else { 0 };
                    let above = prev_row[i] as u16;
                    dst[i] = row_data[i].wrapping_add(((left + above) / 2) as u8);
                }
            }
            4 => {
                // Paeth: each byte uses the Paeth predictor of left, above, upper-left
                for i in 0..row_data.len() {
                    let left = if i >= bpp { dst[i - bpp] as i32 } else { 0 };
                    let above = prev_row[i] as i32;
                    let upper_left = if i >= bpp {
                        prev_row[i - bpp] as i32
                    } else {
                        0
                    };
                    dst[i] = row_data[i].wrapping_add(paeth_predictor(left, above, upper_left));
                }
            }
            _ => {
                // Unknown filter — treat as None
                dst[..row_data.len()].copy_from_slice(row_data);
            }
        }

        prev_row[..bytes_per_row].copy_from_slice(dst);
    }

    result
}

#[inline]
fn paeth_predictor(a: i32, b: i32, c: i32) -> u8 {
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

/// Convert CMYK pixel data to RGB.
///
/// Uses the standard formula: R = 255 × (1-C) × (1-K), etc.
fn cmyk_to_rgb(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = (width * height) as usize;
    let mut rgb = Vec::with_capacity(pixel_count * 3);
    for i in 0..pixel_count {
        let offset = i * 4;
        if offset + 3 >= data.len() {
            break;
        }
        let c = data[offset] as f32 / 255.0;
        let m = data[offset + 1] as f32 / 255.0;
        let y = data[offset + 2] as f32 / 255.0;
        let k = data[offset + 3] as f32 / 255.0;
        rgb.push((255.0 * (1.0 - c) * (1.0 - k)) as u8);
        rgb.push((255.0 * (1.0 - m) * (1.0 - k)) as u8);
        rgb.push((255.0 * (1.0 - y) * (1.0 - k)) as u8);
    }
    rgb
}
