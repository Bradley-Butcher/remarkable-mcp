use remarkable_lines::shared::{pen_color::PenColor, tool::Tool};

#[derive(Debug, Clone)]
pub(crate) struct InkPoint {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) diameter: f32,
    pub(crate) pressure: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct InkStroke {
    pub(crate) points: Vec<InkPoint>,
    pub(crate) tool: Tool,
    pub(crate) color: PenColor,
}
