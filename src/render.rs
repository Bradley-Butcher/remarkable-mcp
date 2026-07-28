use std::sync::Arc;

use image::{DynamicImage, GenericImageView, ImageBuffer, Rgba, imageops::FilterType};
use pdf_render::{RenderSettings, pdf_interpret::InterpreterSettings, pdf_syntax::Pdf};
use remarkable_render::render_ink;
use serde_json::Value;
use tiny_skia::{Color, Pixmap};

use crate::{
    cloud::CloudClient,
    error::{Error, Result},
    model::{
        Crop, Detail, HIGH_MAX_BYTES, HIGH_MAX_EDGE, Item, ItemKind, ReadInput, ReadMetadata,
        RenderedPage, STANDARD_MAX_BYTES, STANDARD_MAX_EDGE,
    },
};

const PAPER_WIDTH: f32 = 1_404.0;
const PAPER_HEIGHT: f32 = 1_872.0;

#[derive(Clone)]
pub struct PageRenderer {
    cloud: CloudClient,
}

impl PageRenderer {
    pub fn new(cloud: CloudClient) -> Self {
        Self { cloud }
    }

    pub async fn read(&self, input: ReadInput) -> Result<RenderedPage> {
        let (library, index) = self.cloud.resolve(&input.document, false).await?;
        let item = library.items[index].clone();
        if item.is_folder() {
            return Err(Error::InvalidInput("a folder cannot be read".into()));
        }
        let document = library.path_for(index);
        let content = self.cloud.file_ending_with(&item, ".content").await?;
        let source = match item.kind {
            ItemKind::Pdf => Some(self.cloud.source_bytes(&item).await?),
            _ => None,
        };
        let pages = page_records(content.as_deref());
        let total_pages = total_pages(&item, source.as_deref(), &pages)?;
        if input.page == 0 || input.page > total_pages {
            return Err(Error::InvalidInput(format!(
                "page {} is outside 1-{total_pages}",
                input.page
            )));
        }

        let page_record = pages.get((input.page - 1) as usize).cloned();
        let rm_suffix = page_record
            .as_ref()
            .filter(|record| !record.id.is_empty())
            .map(|record| format!("{}.rm", record.id))
            .or_else(|| {
                item.files
                    .iter()
                    .filter(|entry| entry.id.ends_with(".rm"))
                    .nth((input.page - 1) as usize)
                    .map(|entry| entry.id.clone())
            });
        let rm_bytes = match rm_suffix {
            Some(suffix) => self.cloud.file_ending_with(&item, &suffix).await?,
            None => None,
        };
        let page = input.page;
        let detail = input.detail;
        let crop = input.crop;
        let kind = item.kind;
        let rendered = tokio::task::spawn_blocking(move || {
            render_page(kind, source, rm_bytes, page, page_record, detail, crop)
        })
        .await
        .map_err(|error| Error::Render(error.to_string()))??;

        Ok(RenderedPage {
            bytes: rendered,
            mime_type: "image/jpeg",
            metadata: ReadMetadata {
                document,
                page,
                total_pages,
            },
        })
    }
}

#[derive(Debug, Clone)]
struct PageRecord {
    id: String,
    pdf_page: Option<usize>,
}

fn page_records(content: Option<&[u8]>) -> Vec<PageRecord> {
    let Some(value) = content.and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok()) else {
        return Vec::new();
    };
    if let Some(entries) = value.pointer("/cPages/pages").and_then(Value::as_array) {
        return entries
            .iter()
            .filter_map(|entry| {
                Some(PageRecord {
                    id: entry.get("id")?.as_str()?.to_owned(),
                    pdf_page: entry
                        .pointer("/redir/value")
                        .and_then(Value::as_u64)
                        .map(|value| value as usize),
                })
            })
            .collect();
    }
    value
        .get("pages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, id)| {
            Some(PageRecord {
                id: id.as_str()?.to_owned(),
                pdf_page: Some(index),
            })
        })
        .collect()
}

fn total_pages(item: &Item, pdf: Option<&[u8]>, pages: &[PageRecord]) -> Result<u32> {
    if !pages.is_empty() {
        return Ok(pages.len() as u32);
    }
    if item.kind == ItemKind::Pdf {
        let pdf = Pdf::new(Arc::new(pdf.unwrap_or_default().to_vec()))
            .map_err(|error| Error::Render(format!("could not open PDF: {error:?}")))?;
        return Ok(pdf.pages().len() as u32);
    }
    let rm_pages = item
        .files
        .iter()
        .filter(|entry| entry.id.ends_with(".rm"))
        .count();
    if rm_pages == 0 {
        Err(Error::Unsupported(
            "document contains no renderable pages".into(),
        ))
    } else {
        Ok(rm_pages as u32)
    }
}

fn render_page(
    kind: ItemKind,
    source: Option<Vec<u8>>,
    rm_bytes: Option<Vec<u8>>,
    physical_page: u32,
    record: Option<PageRecord>,
    detail: Detail,
    crop: Option<Crop>,
) -> Result<Vec<u8>> {
    let (max_edge, max_bytes) = match detail {
        Detail::Standard => (STANDARD_MAX_EDGE, STANDARD_MAX_BYTES),
        Detail::High => (HIGH_MAX_EDGE, HIGH_MAX_BYTES),
    };
    let render_edge = crop
        .map(|crop| {
            (max_edge as f32 / crop.width.max(crop.height).max(0.01))
                .round()
                .min(4_096.0) as u32
        })
        .unwrap_or(max_edge);
    let mut canvas = match kind {
        ItemKind::Pdf => {
            let bytes = source.ok_or_else(|| Error::Unsupported("PDF source is missing".into()))?;
            match record {
                Some(PageRecord {
                    pdf_page: Some(pdf_page),
                    ..
                }) => render_pdf(&bytes, pdf_page, render_edge)?,
                Some(_) => blank_page(render_edge)?,
                None => render_pdf(&bytes, (physical_page - 1) as usize, render_edge)?,
            }
        }
        ItemKind::Notebook | ItemKind::Epub => blank_page(render_edge)?,
        ItemKind::Folder => return Err(Error::InvalidInput("a folder cannot be read".into())),
    };
    if let Some(bytes) = rm_bytes
        && let Err(error) = render_ink(&mut canvas, &bytes)
        && kind != ItemKind::Pdf
    {
        return Err(Error::Render(error.to_string()));
    }
    let image = DynamicImage::ImageRgba8(
        ImageBuffer::<Rgba<u8>, _>::from_raw(canvas.width(), canvas.height(), canvas.take())
            .ok_or_else(|| Error::Render("invalid rendered pixel buffer".into()))?,
    );
    let image = apply_crop(image, crop)?;
    encode_bounded(image, max_edge, max_bytes)
}

fn render_pdf(bytes: &[u8], page_index: usize, max_edge: u32) -> Result<Pixmap> {
    use pdf_render::vello_cpu::color::palette::css::WHITE;

    let pdf = Pdf::new(Arc::new(bytes.to_vec()))
        .map_err(|error| Error::Render(format!("could not open PDF: {error:?}")))?;
    let page = pdf
        .pages()
        .get(page_index)
        .ok_or_else(|| Error::InvalidInput("PDF page does not exist".into()))?;
    let (width, height) = page.render_dimensions();
    let scale = max_edge as f32 / width.max(height).max(1.0);
    let rendered = pdf_render::render(
        page,
        &InterpreterSettings::default(),
        &RenderSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
            ..Default::default()
        },
    );
    Pixmap::from_vec(
        rendered.data_as_u8_slice().to_vec(),
        tiny_skia::IntSize::from_wh(rendered.width().into(), rendered.height().into())
            .ok_or_else(|| Error::Render("PDF page has invalid dimensions".into()))?,
    )
    .ok_or_else(|| Error::Render("could not allocate PDF page".into()))
}

fn blank_page(max_edge: u32) -> Result<Pixmap> {
    let width = (max_edge as f32 * PAPER_WIDTH / PAPER_HEIGHT).round() as u32;
    let mut pixmap = Pixmap::new(width, max_edge)
        .ok_or_else(|| Error::Render("could not allocate page".into()))?;
    pixmap.fill(Color::from_rgba8(251, 251, 249, 255));
    Ok(pixmap)
}

fn apply_crop(image: DynamicImage, crop: Option<Crop>) -> Result<DynamicImage> {
    let Some(crop) = crop else { return Ok(image) };
    if crop.x < 0.0
        || crop.y < 0.0
        || crop.width <= 0.0
        || crop.height <= 0.0
        || crop.x + crop.width > 1.0
        || crop.y + crop.height > 1.0
    {
        return Err(Error::InvalidInput(
            "crop must be normalized and remain inside the page".into(),
        ));
    }
    let (width, height) = image.dimensions();
    let x = (crop.x * width as f32).floor() as u32;
    let y = (crop.y * height as f32).floor() as u32;
    let crop_width = (crop.width * width as f32).ceil() as u32;
    let crop_height = (crop.height * height as f32).ceil() as u32;
    Ok(image.crop_imm(
        x,
        y,
        crop_width.min(width - x).max(1),
        crop_height.min(height - y).max(1),
    ))
}

fn encode_bounded(mut image: DynamicImage, max_edge: u32, max_bytes: usize) -> Result<Vec<u8>> {
    image = resize_long_edge(image, max_edge);
    loop {
        for quality in [84, 76, 68, 58, 48] {
            let mut output = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, quality)
                .encode_image(&image)
                .map_err(|error| Error::Render(error.to_string()))?;
            if output.len() <= max_bytes {
                return Ok(output);
            }
        }
        let (width, height) = image.dimensions();
        if width.max(height) <= 320 {
            return Err(Error::Render("page could not fit the image budget".into()));
        }
        image = image.resize(
            (width as f32 * 0.82).round() as u32,
            (height as f32 * 0.82).round() as u32,
            FilterType::Lanczos3,
        );
    }
}

fn resize_long_edge(image: DynamicImage, max_edge: u32) -> DynamicImage {
    let (width, height) = image.dimensions();
    if width.max(height) <= max_edge {
        image
    } else {
        image.resize(max_edge, max_edge, FilterType::Lanczos3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_page_metadata_shapes() {
        let old = page_records(Some(br#"{"pages":["a","b"]}"#));
        assert_eq!(old[1].id, "b");
        assert_eq!(old[1].pdf_page, Some(1));
        let new = page_records(Some(
            br#"{"cPages":{"pages":[{"id":"x","redir":{"value":4}}]}}"#,
        ));
        assert_eq!(new[0].pdf_page, Some(4));
    }

    #[test]
    fn encoded_page_respects_the_hard_budget() {
        let mut pixels = ImageBuffer::new(2200, 1700);
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            *pixel = Rgba([(x % 255) as u8, (y % 255) as u8, ((x + y) % 255) as u8, 255]);
        }
        let encoded = encode_bounded(DynamicImage::ImageRgba8(pixels), 1600, 80_000).unwrap();
        assert!(encoded.len() <= 80_000);
        let decoded = image::load_from_memory(&encoded).unwrap();
        assert!(decoded.width().max(decoded.height()) <= 1600);
    }

    #[test]
    fn renders_a_pdf_page_to_a_bounded_jpeg() {
        use lopdf::{Document, Object, Stream, dictionary};

        let mut document = Document::with_version("1.4");
        let pages_id = document.new_object_id();
        let page_id = document.new_object_id();
        let content_id = document.add_object(Stream::new(dictionary! {}, Vec::new()));
        document.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Page".to_vec()),
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(72), Object::Integer(144),
                ]),
                "Contents" => Object::Reference(content_id),
            }),
        );
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Pages".to_vec()),
                "Kids" => Object::Array(vec![Object::Reference(page_id)]),
                "Count" => Object::Integer(1),
            }),
        );
        let catalog_id = document.new_object_id();
        document.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! {
                "Type" => Object::Name(b"Catalog".to_vec()),
                "Pages" => Object::Reference(pages_id),
            }),
        );
        document.trailer.set("Root", Object::Reference(catalog_id));
        let mut pdf = Vec::new();
        document.save_to(&mut pdf).unwrap();

        let jpeg = render_page(
            ItemKind::Pdf,
            Some(pdf),
            None,
            1,
            None,
            Detail::Standard,
            None,
        )
        .unwrap();
        let image = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(image.height(), STANDARD_MAX_EDGE);
        assert_eq!(image.width(), STANDARD_MAX_EDGE / 2);
        assert!(jpeg.len() <= STANDARD_MAX_BYTES);
    }
}
