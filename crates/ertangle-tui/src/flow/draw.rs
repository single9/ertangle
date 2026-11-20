//! Renders a [`FlowModel`] into the center pane.

use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders};
use ratatui::Frame as TuiFrame;

use super::FlowModel;

/// Draws the Flow view: summary strip, node board, edge ribbon.
///
/// `cursor` is the selected index into `model.nodes` (already clamped by the caller).
pub(crate) fn draw_flow(f: &mut TuiFrame, area: Rect, _model: &FlowModel, _cursor: usize) {
    // T76b: replace this placeholder with the real renderer.
    f.render_widget(Block::default().borders(Borders::ALL).title("Flow"), area);
}
