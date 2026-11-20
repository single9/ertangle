//! Pure derivation of [`FlowModel`] from `App` state (no I/O, no mutation).

use super::FlowModel;
use crate::app::App;

/// Builds the Flow view model for the active host at `now_ms`.
///
/// `now_ms` is a parameter so tests are deterministic; callers pass
/// `ertangle_core::now_millis()`.
pub(crate) fn build_flow_model(_app: &App, now_ms: u64) -> FlowModel {
    // T76a: replace this placeholder with the real derivation.
    FlowModel {
        now_ms,
        ..FlowModel::default()
    }
}
