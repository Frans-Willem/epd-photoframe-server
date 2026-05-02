//! Overlay trait system: each screen overlay declares an async
//! [`Overlay::preprocess`] step that runs in parallel with photo
//! retrieval, then a synchronous [`ReadyOverlay::render`] step that
//! draws onto the shared screen [`tiny_skia::Pixmap`]. See `PLAN.md`
//! Phase 1 for the full design.
//!
//! Concrete overlays live as sub-modules. Drawing primitives
//! ([`crate::draw`]) are intentionally kept top-level since
//! `degraded.rs` uses them too and isn't itself an overlay.

mod battery_indicator;
mod infobox;
mod traits;

pub use battery_indicator::BatteryIndicator;
pub use infobox::Infobox;
pub use traits::{Overlay, OverlayContext, ReadyOverlay};

use crate::PowerState;

/// Snapshot of sensor readings forwarded by the device, captured per
/// request. Each field is `Option` because the device may report any
/// subset of these on any given request. Some fields aren't yet
/// consumed by any overlay — they're carried because they will be
/// (calendar / notification overlays, future weather variants, etc.).
#[allow(dead_code)]
#[derive(Debug, Default, Clone, Copy)]
pub struct SensorState {
    pub battery_mv: Option<u32>,
    pub battery_pct: Option<u8>,
    pub temperature_c: Option<f32>,
    pub humidity_pct: Option<f32>,
    pub power: Option<PowerState>,
}
