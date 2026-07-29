use remarkable_lines::shared::tool::Tool;
use tiny_skia::{BlendMode, Color, Paint, Pixmap, PixmapPaint, Rect, Transform};

use crate::{
    RenderError, RenderStats,
    geometry::stroke_mask,
    model::InkStroke,
    style::{ink_color, stroke_alpha},
};

pub(crate) fn render_strokes(
    canvas: &mut Pixmap,
    strokes: &[InkStroke],
) -> Result<RenderStats, RenderError> {
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

#[cfg(test)]
mod tests {
    use remarkable_lines::shared::pen_color::PenColor;

    use super::*;
    use crate::model::{InkPoint, InkStroke};

    fn point(x: f32, y: f32, diameter: f32) -> InkPoint {
        InkPoint {
            x,
            y,
            diameter,
            pressure: 0.5,
        }
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
