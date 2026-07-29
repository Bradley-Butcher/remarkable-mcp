use remarkable_lines::shared::{pen_color::PenColor, tool::Tool};

use crate::model::InkStroke;

pub(crate) fn stroke_alpha(stroke: &InkStroke) -> u8 {
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

pub(crate) fn ink_color(color: &PenColor) -> (u8, u8, u8) {
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
