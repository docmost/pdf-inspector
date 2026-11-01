//! Docmost addition: embedded image extraction.

use flate2::write::ZlibEncoder;
use flate2::Compression;
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};
use pdf_inspector::{
    extract_images_mem, process_pdf_mem_with_options, ImageFormat, PdfOptions, PdfProcessResult,
};
use std::io::Write;

const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn jpeg(marker: u8) -> Vec<u8> {
    vec![0xFF, 0xD8, 0xFF, 0xE0, marker, 0xFF, 0xD9]
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

struct PdfBuilder {
    doc: Document,
    pages_id: ObjectId,
    font_id: ObjectId,
    kids: Vec<Object>,
}

impl PdfBuilder {
    fn new() -> Self {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        Self {
            doc,
            pages_id,
            font_id,
            kids: Vec::new(),
        }
    }

    fn image(&mut self, mut dict: Dictionary, data: Vec<u8>) -> ObjectId {
        dict.set("Type", "XObject");
        dict.set("Subtype", "Image");
        dict.set("Width", 2);
        dict.set("Height", 2);
        dict.set("BitsPerComponent", 8);
        self.doc.add_object(Stream::new(dict, data))
    }

    fn jpeg_image(&mut self, data: Vec<u8>) -> ObjectId {
        let dict = dictionary! { "Filter" => "DCTDecode", "ColorSpace" => "DeviceRGB" };
        self.image(dict, data)
    }

    fn form(&mut self, xobjects: Dictionary, painting: &str) -> ObjectId {
        let dict = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! { "XObject" => xobjects },
        };
        self.doc
            .add_object(Stream::new(dict, painting.as_bytes().to_vec()))
    }

    fn page(&mut self, xobjects: Dictionary, painting: &str) {
        let body: String = (0..12)
            .map(|line| {
                let y = 760 - line * 14;
                format!("BT /F1 12 Tf 72 {y} Td (Page body text, line {line} of the words above the image) Tj ET\n")
            })
            .collect();
        let content = format!(
            "{body}BT /F1 12 Tf 72 400 Td (A second line further down that page) Tj ET\n{painting}"
        );
        let content_id = self
            .doc
            .add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page_id = self.doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => self.pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => self.font_id },
                "XObject" => xobjects,
            },
        });
        self.kids.push(page_id.into());
    }

    fn build(mut self) -> Vec<u8> {
        let count = self.kids.len() as i64;
        self.doc.objects.insert(
            self.pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => self.kids,
                "Count" => count,
            }),
        );
        let catalog_id = self.doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => self.pages_id,
        });
        self.doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        self.doc.save_to(&mut bytes).unwrap();
        bytes
    }
}

const PAINT_IM0: &str = "q 100 0 0 100 72 500 cm /Im0 Do Q";

fn with_images(pdf: &[u8]) -> PdfProcessResult {
    process_pdf_mem_with_options(pdf, PdfOptions::new().extract_images(true)).unwrap()
}

fn markdown(result: &PdfProcessResult) -> &str {
    result.markdown.as_deref().unwrap()
}

#[test]
fn jpeg_is_returned_verbatim_with_a_placeholder() {
    let mut pdf = PdfBuilder::new();
    let im0 = pdf.jpeg_image(jpeg(1));
    pdf.page(dictionary! { "Im0" => im0 }, PAINT_IM0);

    let result = with_images(&pdf.build());

    assert_eq!(result.images.len(), 1);
    assert!(matches!(result.images[0].format, ImageFormat::Jpeg));
    assert_eq!(result.images[0].data, jpeg(1));
    assert_eq!(result.images[0].page, 1);
    assert_eq!(
        (result.images[0].width, result.images[0].height),
        (2, 2),
        "pixel size comes from the image dictionary"
    );
    assert!(markdown(&result).contains("![image](pdf-image://0)"));
}

#[test]
fn placeholder_sits_between_the_lines_around_the_image() {
    let mut pdf = PdfBuilder::new();
    let im0 = pdf.jpeg_image(jpeg(1));
    pdf.page(dictionary! { "Im0" => im0 }, PAINT_IM0);

    let result = with_images(&pdf.build());
    let markdown = markdown(&result);

    let first = markdown.find("Page body text").unwrap();
    let image = markdown.find("pdf-image://0").unwrap();
    let second = markdown.find("A second line").unwrap();
    assert!(first < image && image < second, "{markdown}");
}

#[test]
fn flate_rgb_is_encoded_as_png() {
    let mut pdf = PdfBuilder::new();
    let pixels: Vec<u8> = (0..12).collect();
    let dict = dictionary! { "Filter" => "FlateDecode", "ColorSpace" => "DeviceRGB" };
    let im0 = pdf.image(dict, zlib(&pixels));
    pdf.page(dictionary! { "Im0" => im0 }, PAINT_IM0);

    let result = with_images(&pdf.build());

    assert_eq!(result.images.len(), 1);
    assert!(matches!(result.images[0].format, ImageFormat::Png));
    assert!(result.images[0].data.starts_with(PNG_SIGNATURE));
}

#[test]
fn chained_filters_unwrap_to_the_jpeg() {
    let mut pdf = PdfBuilder::new();
    let hex: String = jpeg(7).iter().map(|b| format!("{b:02X}")).collect();
    let dict = dictionary! {
        "Filter" => vec!["ASCIIHexDecode".into(), "DCTDecode".into()],
        "ColorSpace" => "DeviceRGB",
    };
    let im0 = pdf.image(dict, format!("{hex}>").into_bytes());
    pdf.page(dictionary! { "Im0" => im0 }, PAINT_IM0);

    let result = with_images(&pdf.build());

    assert_eq!(result.images.len(), 1);
    assert_eq!(result.images[0].data, jpeg(7));
}

#[test]
fn nothing_is_extracted_unless_asked() {
    let mut pdf = PdfBuilder::new();
    let im0 = pdf.jpeg_image(jpeg(1));
    pdf.page(dictionary! { "Im0" => im0 }, PAINT_IM0);

    let result = process_pdf_mem_with_options(&pdf.build(), PdfOptions::new()).unwrap();

    assert!(result.images.is_empty());
    assert!(!markdown(&result).contains("!["), "{}", markdown(&result));
}

#[test]
fn unsupported_image_gets_no_placeholder() {
    let mut pdf = PdfBuilder::new();
    let dict = dictionary! { "Filter" => "JPXDecode", "ColorSpace" => "DeviceRGB" };
    let im0 = pdf.image(dict, vec![0; 16]);
    pdf.page(dictionary! { "Im0" => im0 }, PAINT_IM0);

    let result = with_images(&pdf.build());

    assert!(result.images.is_empty());
    assert!(!markdown(&result).contains("!["), "{}", markdown(&result));
    assert!(markdown(&result).contains("Page body text"));
}

#[test]
fn indices_skip_unsupported_images() {
    let mut pdf = PdfBuilder::new();
    let dict = dictionary! { "Filter" => "JPXDecode", "ColorSpace" => "DeviceRGB" };
    let unsupported = pdf.image(dict, vec![0; 16]);
    let supported = pdf.jpeg_image(jpeg(2));
    pdf.page(
        dictionary! { "Im0" => unsupported, "Im1" => supported },
        "q 100 0 0 100 72 600 cm /Im0 Do Q q 100 0 0 100 72 500 cm /Im1 Do Q",
    );

    let result = with_images(&pdf.build());

    assert_eq!(result.images.len(), 1);
    assert_eq!(result.images[0].data, jpeg(2));
    assert!(markdown(&result).contains("pdf-image://0"));
    assert!(!markdown(&result).contains("pdf-image://1"));
}

#[test]
fn indices_run_across_pages() {
    let mut pdf = PdfBuilder::new();
    let first = pdf.jpeg_image(jpeg(1));
    let second = pdf.jpeg_image(jpeg(2));
    pdf.page(dictionary! { "Im0" => first }, PAINT_IM0);
    pdf.page(dictionary! { "Im0" => second }, PAINT_IM0);

    let result = with_images(&pdf.build());

    let pages: Vec<u32> = result.images.iter().map(|image| image.page).collect();
    assert_eq!(pages, [1, 2]);
    assert_eq!(result.images[1].data, jpeg(2));
    assert!(markdown(&result).contains("pdf-image://0"));
    assert!(markdown(&result).contains("pdf-image://1"));
}

#[test]
fn page_filter_limits_images_and_numbers_from_zero() {
    let mut pdf = PdfBuilder::new();
    let first = pdf.jpeg_image(jpeg(1));
    let second = pdf.jpeg_image(jpeg(2));
    pdf.page(dictionary! { "Im0" => first }, PAINT_IM0);
    pdf.page(dictionary! { "Im0" => second }, PAINT_IM0);

    let options = PdfOptions::new().extract_images(true).pages([2]);
    let result = process_pdf_mem_with_options(&pdf.build(), options).unwrap();

    assert_eq!(result.images.len(), 1);
    assert_eq!(result.images[0].page, 2);
    assert_eq!(result.images[0].data, jpeg(2));
    assert!(markdown(&result).contains("pdf-image://0"));
}

// The same resource name means a different image in each Form XObject, so the
// name alone cannot identify the picture a placeholder stands for.
#[test]
fn images_in_form_xobjects_resolve_in_the_form_namespace() {
    let mut pdf = PdfBuilder::new();
    let on_page = pdf.jpeg_image(jpeg(1));
    let in_form = pdf.jpeg_image(jpeg(2));
    let form = pdf.form(dictionary! { "Im0" => in_form }, PAINT_IM0);
    pdf.page(
        dictionary! { "Im0" => on_page, "Fm0" => form },
        "q 100 0 0 100 72 600 cm /Im0 Do Q q 1 0 0 1 0 -200 cm /Fm0 Do Q",
    );

    let result = with_images(&pdf.build());

    assert_eq!(result.images.len(), 2);
    let mut data: Vec<&Vec<u8>> = result.images.iter().map(|image| &image.data).collect();
    data.sort();
    assert_eq!(data, [&jpeg(1), &jpeg(2)]);
}

#[test]
fn repeated_image_gets_a_placeholder_per_paint() {
    let mut pdf = PdfBuilder::new();
    let im0 = pdf.jpeg_image(jpeg(1));
    pdf.page(
        dictionary! { "Im0" => im0 },
        "q 100 0 0 100 72 600 cm /Im0 Do Q q 100 0 0 100 72 500 cm /Im0 Do Q",
    );

    let result = with_images(&pdf.build());

    assert_eq!(result.images.len(), 2);
    assert!(markdown(&result).contains("pdf-image://0"));
    assert!(markdown(&result).contains("pdf-image://1"));
}

#[test]
fn extract_images_mem_returns_the_same_images() {
    let mut pdf = PdfBuilder::new();
    let im0 = pdf.jpeg_image(jpeg(1));
    pdf.page(dictionary! { "Im0" => im0 }, PAINT_IM0);
    let bytes = pdf.build();

    let images = extract_images_mem(&bytes).unwrap();

    assert_eq!(images.len(), 1);
    assert_eq!(images[0].data, jpeg(1));
}

#[test]
fn image_free_document_returns_no_images() {
    let mut pdf = PdfBuilder::new();
    pdf.page(dictionary! {}, "");

    let result = with_images(&pdf.build());

    assert!(result.images.is_empty());
    assert!(!markdown(&result).contains("pdf-image://"));
}
