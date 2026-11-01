//! Docmost addition: embedded image extraction bindings.

use super::{catch_panic, to_napi_err, to_napi_result, PdfType};
use napi::bindgen_prelude::*;
use napi_derive::napi;

/// Image format for extracted images.
#[napi(string_enum)]
pub enum ImageFormat {
    Jpeg,
    Png,
}

/// An image extracted from a PDF.
#[napi(object)]
pub struct ExtractedImage {
    /// Page number (1-indexed).
    pub page: u32,
    /// X position on page.
    pub x: f64,
    /// Y position on page.
    pub y: f64,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Image format.
    pub format: ImageFormat,
    /// Raw image bytes.
    pub data: Buffer,
}

/// Full PDF processing result with markdown, metadata, and extracted images.
///
/// The markdown contains `![image](pdf-image://N)` placeholders where N
/// is the index into the `images` array.
#[napi(object)]
pub struct PdfResultWithImages {
    pub pdf_type: PdfType,
    pub markdown: Option<String>,
    pub page_count: u32,
    pub processing_time_ms: u32,
    pub pages_needing_ocr: Vec<u32>,
    pub title: Option<String>,
    pub confidence: f64,
    pub is_complex_layout: bool,
    pub pages_with_tables: Vec<u32>,
    pub pages_with_columns: Vec<u32>,
    pub has_encoding_issues: bool,
    /// Extracted images. Indices match `pdf-image://N` placeholders in markdown.
    pub images: Vec<ExtractedImage>,
}

fn to_napi_images(images: Vec<pdf_inspector::ExtractedImage>) -> Vec<ExtractedImage> {
    images
        .into_iter()
        .map(|img| ExtractedImage {
            page: img.page,
            x: img.x as f64,
            y: img.y as f64,
            width: img.width,
            height: img.height,
            format: match img.format {
                pdf_inspector::ImageFormat::Jpeg => ImageFormat::Jpeg,
                pdf_inspector::ImageFormat::Png => ImageFormat::Png,
            },
            data: img.data.into(),
        })
        .collect()
}

/// Process a PDF and extract both markdown and images.
///
/// The markdown contains `![image](pdf-image://N)` placeholders where N is the
/// index into the returned `images` array.
#[napi]
pub fn process_pdf_with_images(
    buffer: Buffer,
    pages: Option<Vec<u32>>,
) -> Result<PdfResultWithImages> {
    let bytes: Vec<u8> = buffer.to_vec();
    catch_panic("process_pdf_with_images", move || {
        let mut opts = pdf_inspector::PdfOptions::new().extract_images(true);
        if let Some(p) = pages {
            opts = opts.pages(p);
        }
        let mut result = pdf_inspector::process_pdf_mem_with_options(&bytes, opts)
            .map_err(|e| to_napi_err(e, "process_pdf_with_images"))?;
        let images = to_napi_images(std::mem::take(&mut result.images));
        let result = to_napi_result(result);
        Ok(PdfResultWithImages {
            pdf_type: result.pdf_type,
            markdown: result.markdown,
            page_count: result.page_count,
            processing_time_ms: result.processing_time_ms,
            pages_needing_ocr: result.pages_needing_ocr,
            title: result.title,
            confidence: result.confidence,
            is_complex_layout: result.is_complex_layout,
            pages_with_tables: result.pages_with_tables,
            pages_with_columns: result.pages_with_columns,
            has_encoding_issues: result.has_encoding_issues,
            images,
        })
    })
}

/// Extract images from a PDF Buffer.
///
/// Extracts JPEG (DCTDecode) and PNG (FlateDecode) images.
/// Returns raw image bytes with position and dimension metadata.
#[napi]
pub fn extract_images(buffer: Buffer) -> Result<Vec<ExtractedImage>> {
    let bytes: Vec<u8> = buffer.to_vec();
    catch_panic("extract_images", move || {
        let images = pdf_inspector::extract_images_mem(&bytes)
            .map_err(|e| to_napi_err(e, "extract_images"))?;
        Ok(to_napi_images(images))
    })
}
