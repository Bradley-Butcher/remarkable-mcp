//! Rendering for reMarkable `.rm` ink layers.
//!
//! The parser's per-point width and pressure data are retained all the way to
//! rasterisation. Ink is rendered to a transparent layer first, which lets
//! erasers remove ink without painting over a PDF or template background.

use remarkable_lines::{
    RemarkableFile,
    shared::{pen_color::PenColor, tool::Tool},
    v6::block::Block,
};
use tiny_skia::{
    BlendMode, Color, FillRule, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Transform,
};

const PAPER_WIDTH: f32 = 1_404.0;
const PAPER_HEIGHT: f32 = 1_872.0;
const V6_WIDTH_SCALE: f32 = 4.0;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("could not parse ink layer: {0}")]
    Parse(#[from] remarkable_lines::ParseError),
    #[error("could not allocate ink layer")]
    Allocation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderStats {
    pub strokes: usize,
    pub erased_strokes: usize,
}

#[derive(Debug, Clone)]
struct InkPoint {
    x: f32,
    y: f32,
    diameter: f32,
    pressure: f32,
}

#[derive(Debug, Clone)]
struct InkStroke {
    points: Vec<InkPoint>,
    tool: Tool,
    color: PenColor,
}

/// Paints a parsed reMarkable ink layer over an existing page background.
pub fn render_ink(canvas: &mut Pixmap, bytes: &[u8]) -> Result<RenderStats, RenderError> {
    let strokes = parse_strokes(bytes)?;
    render_strokes(canvas, &strokes)
}

fn parse_strokes(bytes: &[u8]) -> Result<Vec<InkStroke>, RenderError> {
    let file = RemarkableFile::read(bytes)?;
    let mut output = Vec::new();
    match file {
        RemarkableFile::V6 { blocks, .. } => {
            for block in blocks {
                let Block::SceneLineItem(block) = block else {
                    continue;
                };
                if block.item.deleted_length != 0 {
                    continue;
                }
                let Some(line) = block.item.value else {
                    continue;
                };
                output.push(InkStroke {
                    points: line
                        .points
                        .into_iter()
                        .map(|point| InkPoint {
                            x: point.x,
                            y: point.y,
                            // Native strokev2 converts the packed point width with
                            // a fixed 0.25 factor. `thickness_scale` is carried as a
                            // separate pipeline varying; it is not geometric width.
                            diameter: native_v6_diameter(point.width),
                            pressure: point.pressure / 255.0,
                        })
                        .collect(),
                    tool: line.tool,
                    color: line.color,
                });
            }
        }
        RemarkableFile::Other { pages, .. } => {
            for line in pages
                .into_iter()
                .flat_map(|page| page.layers)
                .flat_map(|layer| layer.lines)
            {
                let brush_size = line.brush_size;
                output.push(InkStroke {
                    points: line
                        .points
                        .into_iter()
                        .map(|point| InkPoint {
                            x: point.x,
                            y: point.y,
                            diameter: if point.width > 0.0 {
                                point.width
                            } else {
                                brush_size
                            },
                            pressure: point.pressure.clamp(0.0, 1.0),
                        })
                        .collect(),
                    tool: line.tool,
                    color: line.color,
                });
            }
        }
    }
    Ok(output)
}

fn render_strokes(canvas: &mut Pixmap, strokes: &[InkStroke]) -> Result<RenderStats, RenderError> {
    let mut ink = Pixmap::new(canvas.width(), canvas.height()).ok_or(RenderError::Allocation)?;
    let full_page = Rect::from_xywh(0.0, 0.0, canvas.width() as f32, canvas.height() as f32)
        .ok_or(RenderError::Allocation)?;
    let mut stats = RenderStats::default();

    for stroke in strokes {
        if matches!(stroke.tool, Tool::SelectionBrush) || stroke.points.is_empty() {
            continue;
        }
        if matches!(stroke.tool, Tool::EraseAll) {
            ink.fill(Color::TRANSPARENT);
            stats.erased_strokes += 1;
            continue;
        }
        let Some(mask) = stroke_mask(stroke, canvas.width(), canvas.height()) else {
            continue;
        };
        let mut paint = Paint {
            anti_alias: true,
            ..Default::default()
        };
        if matches!(stroke.tool, Tool::Eraser | Tool::EraseArea) {
            paint.set_color_rgba8(0, 0, 0, 255);
            paint.blend_mode = BlendMode::DestinationOut;
            stats.erased_strokes += 1;
        } else {
            let (red, green, blue) = ink_color(&stroke.color);
            paint.set_color_rgba8(red, green, blue, stroke_alpha(stroke));
            if matches!(stroke.tool, Tool::Highlighter) {
                paint.blend_mode = BlendMode::Multiply;
            }
            stats.strokes += 1;
        }
        ink.fill_rect(full_page, &paint, Transform::identity(), Some(&mask));
    }

    canvas.draw_pixmap(
        0,
        0,
        ink.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    Ok(stats)
}

fn native_v6_diameter(stored_width: f32) -> f32 {
    stored_width / V6_WIDTH_SCALE
}

fn stroke_mask(stroke: &InkStroke, width: u32, height: u32) -> Option<Mask> {
    let mut mask = Mask::new(width, height)?;
    let scale = height as f32 / PAPER_HEIGHT;
    let points: Vec<(f32, f32, f32)> = stroke
        .points
        .iter()
        .map(|point| {
            (
                map_x(point.x, width),
                map_y(point.y, height),
                (point.diameter * scale).clamp(0.55, 72.0) / 2.0,
            )
        })
        .collect();

    if points.len() == 1 {
        let (x, y, radius) = points[0];
        let circle = PathBuilder::from_circle(x, y, radius)?;
        mask.fill_path(&circle, FillRule::Winding, true, Transform::identity());
        return Some(mask);
    }

    let mut left = Vec::with_capacity(points.len());
    let mut right = Vec::with_capacity(points.len());
    for index in 0..points.len() {
        let previous = points[index.saturating_sub(1)];
        let next = points[(index + 1).min(points.len() - 1)];
        let dx = next.0 - previous.0;
        let dy = next.1 - previous.1;
        let length = dx.hypot(dy).max(0.001);
        let normal_x = -dy / length;
        let normal_y = dx / length;
        let (x, y, radius) = points[index];
        left.push((x + normal_x * radius, y + normal_y * radius));
        right.push((x - normal_x * radius, y - normal_y * radius));
    }

    let mut path = PathBuilder::new();
    path.move_to(left[0].0, left[0].1);
    for point in left.iter().skip(1) {
        path.line_to(point.0, point.1);
    }
    for point in right.iter().rev() {
        path.line_to(point.0, point.1);
    }
    path.close();
    if let Some(path) = path.finish() {
        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
    }
    for &(x, y, radius) in [points[0], points[points.len() - 1]].iter() {
        if let Some(circle) = PathBuilder::from_circle(x, y, radius) {
            mask.fill_path(&circle, FillRule::Winding, true, Transform::identity());
        }
    }
    Some(mask)
}

fn stroke_alpha(stroke: &InkStroke) -> u8 {
    if matches!(stroke.tool, Tool::Highlighter) {
        return 92;
    }
    let pressure = stroke
        .points
        .iter()
        .map(|point| point.pressure)
        .sum::<f32>()
        / stroke.points.len().max(1) as f32;
    match stroke.tool {
        Tool::Pencil => (155.0 + pressure.clamp(0.0, 1.0) * 80.0) as u8,
        Tool::MechanicalPencil => 225,
        Tool::Brush | Tool::Calligraphy => (190.0 + pressure.clamp(0.0, 1.0) * 65.0) as u8,
        _ => 255,
    }
}

fn ink_color(color: &PenColor) -> (u8, u8, u8) {
    match color {
        PenColor::Black => (25, 25, 25),
        PenColor::Grey | PenColor::GreyOverlap => (112, 112, 112),
        PenColor::White => (251, 251, 249),
        PenColor::Yellow => (245, 209, 66),
        PenColor::Green => (83, 157, 94),
        PenColor::Pink => (225, 110, 160),
        PenColor::Blue => (54, 111, 190),
        PenColor::Red => (198, 61, 55),
    }
}

fn map_x(x: f32, width: u32) -> f32 {
    ((x + PAPER_WIDTH / 2.0) / PAPER_WIDTH) * width as f32
}

fn map_y(y: f32, height: u32) -> f32 {
    (y / PAPER_HEIGHT) * height as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f32, y: f32, diameter: f32) -> InkPoint {
        InkPoint {
            x,
            y,
            diameter,
            pressure: 0.5,
        }
    }

    #[test]
    fn variable_width_geometry_really_changes_width() {
        let stroke = InkStroke {
            points: vec![
                point(-500.0, 400.0, 4.0),
                point(0.0, 400.0, 16.0),
                point(500.0, 400.0, 28.0),
            ],
            tool: Tool::Marker,
            color: PenColor::Black,
        };
        let mask = stroke_mask(&stroke, 1404, 1872).unwrap();
        let data = mask.data();
        let column_ink = |x: usize| (0..1872).filter(|y| data[y * 1404 + x] > 0).count();
        assert!(column_ink(702) > column_ink(202) * 2);
        assert!(column_ink(1202) > column_ink(702));
    }

    #[test]
    fn v6_width_uses_the_native_quarter_unit_conversion() {
        assert_eq!(native_v6_diameter(24.0), 6.0);
    }

    #[test]
    fn eraser_removes_ink_without_repainting_background() {
        let mut canvas = Pixmap::new(1404, 1872).unwrap();
        canvas.fill(Color::from_rgba8(40, 90, 140, 255));
        let strokes = vec![
            InkStroke {
                points: vec![point(-300.0, 700.0, 80.0), point(300.0, 700.0, 80.0)],
                tool: Tool::Marker,
                color: PenColor::Black,
            },
            InkStroke {
                points: vec![point(0.0, 650.0, 100.0), point(0.0, 750.0, 100.0)],
                tool: Tool::Eraser,
                color: PenColor::Black,
            },
        ];
        render_strokes(&mut canvas, &strokes).unwrap();
        let center = canvas.pixel(702, 700).unwrap();
        assert_eq!((center.red(), center.green(), center.blue()), (40, 90, 140));
        let ink = canvas.pixel(500, 700).unwrap();
        assert_ne!((ink.red(), ink.green(), ink.blue()), (40, 90, 140));
    }
}
