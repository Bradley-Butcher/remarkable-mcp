//! Rendering for reMarkable `.rm` ink layers.
//!
//! The parser's per-point width and pressure data are retained all the way to
//! rasterisation. Ink is rendered to a transparent layer first, which lets
//! erasers remove ink without painting over a PDF or template background.

mod compositor;
mod geometry;
mod model;
mod parse;
mod style;

use tiny_skia::Pixmap;

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

/// Paints a parsed reMarkable ink layer over an existing page background.
pub fn render_ink(canvas: &mut Pixmap, bytes: &[u8]) -> Result<RenderStats, RenderError> {
    let strokes = parse::parse_strokes(bytes)?;
    compositor::render_strokes(canvas, &strokes)
}
