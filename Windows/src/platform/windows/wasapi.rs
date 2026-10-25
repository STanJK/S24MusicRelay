//! WASAPI render adapter.
//!
//! This will be the render-clock authority and will request output frames from
//! the Media pipeline. No UDP or source-clock policy belongs here.

#[derive(Debug, Default)]
pub struct WasapiRenderSink;
