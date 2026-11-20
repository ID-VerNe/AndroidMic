//! On-demand capture (issue #3, PR2): the PC only asks the phone to open its
//! microphone while a real local client is actively recording from the monitored
//! virtual-cable capture endpoint.
//!
//! Everything here fails open: any enumeration failure, missing endpoint,
//! unsupported platform, or negotiation timeout degrades to the legacy
//! always-on behaviour so existing audio playback is never blocked by this
//! feature.

mod config;
mod endpoint;
mod source;
mod state;
mod watcher;

#[cfg(target_os = "windows")]
mod source_windows;

pub use config::OnDemandCaptureConfig;
pub use endpoint::{DEFAULT_ENDPOINT_HINT, select_endpoint};
pub use source::{SessionSource, create_source};
pub use state::{Consumer, DemandMachine, WatcherError};
pub use watcher::{WatcherStream, WatcherUpdate, spawn_watcher, POLL_INTERVAL};
