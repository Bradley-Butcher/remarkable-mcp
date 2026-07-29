use remarkable_lines::{RemarkableFile, v6::block::Block};

use crate::{
    RenderError,
    model::{InkPoint, InkStroke},
};

const V6_WIDTH_SCALE: f32 = 4.0;

pub(crate) fn parse_strokes(bytes: &[u8]) -> Result<Vec<InkStroke>, RenderError> {
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

fn native_v6_diameter(stored_width: f32) -> f32 {
    stored_width / V6_WIDTH_SCALE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v6_width_uses_the_native_quarter_unit_conversion() {
        assert_eq!(native_v6_diameter(24.0), 6.0);
    }
}
