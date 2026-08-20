//! RMS product runtime built around the Rerun Viewer.
//!
//! This crate owns the operator UI and embeds [`re_viewer::App`] directly.
//! Both native and web builds therefore use one `EntityDB`, time controller,
//! renderer, selection model, and blueprint runtime.

mod product_app;

#[cfg(target_arch = "wasm32")]
mod web;

pub use product_app::{
    LiveViewerContext, ReplayViewerContext, RmsControlEvent, RmsControlEventSink,
    RmsControlResponse, RmsHostEvent, RmsHostEventSink, RmsProductApp, RmsSourceKind,
    RmsTopicContext, RmsViewerContext,
};

#[cfg(target_arch = "wasm32")]
pub use web::RmsWebHandle;

/// Development recording used until a project-specific source is supplied by the RMS backend.
pub const DEFAULT_RECORDING_URL: &str =
    "https://app.rerun.io/version/0.36.1/examples/arkit_scenes.rrd";
