use tiny_skia::{FillRule, Mask, PathBuilder, Transform};

use crate::model::InkStroke;

const PAPER_WIDTH: f32 = 1_404.0;
const PAPER_HEIGHT: f32 = 1_872.0;

pub(crate) fn stroke_mask(stroke: &InkStroke, width: u32, height: u32) -> Option<Mask> {
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

fn map_x(x: f32, width: u32) -> f32 {
    ((x + PAPER_WIDTH / 2.0) / PAPER_WIDTH) * width as f32
}

fn map_y(y: f32, height: u32) -> f32 {
    (y / PAPER_HEIGHT) * height as f32
}

#[cfg(test)]
mod tests {
    use remarkable_lines::shared::{pen_color::PenColor, tool::Tool};

    use super::*;
    use crate::model::InkPoint;

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
}
