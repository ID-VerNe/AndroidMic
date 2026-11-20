//! Session source abstraction.
//!
//! A source lists the clients currently recording from the monitored capture
//! endpoint. The Windows WASAPI implementation lives in `source_windows`;
//! every other platform returns [`WatcherError::UnsupportedPlatform`], which
//! the watcher turns into a fail-open degraded state.

use super::{Consumer, OnDemandCaptureConfig, WatcherError};

/// Periodic snapshot provider. The source is constructed and used entirely on
/// the dedicated watcher thread, so it does not need to cross threads.
pub trait SessionSource {
    /// Snapshot the active consumers. Implementations must never panic:
    /// COM/device failures are reported as [`WatcherError`] instead.
    fn snapshot(&mut self) -> Result<Vec<Consumer>, WatcherError>;
}

/// Construct the platform source. Fails open with a [`WatcherError`] the
/// caller surfaces as degraded always-on.
pub fn create_source(
    config: &OnDemandCaptureConfig,
) -> Result<Box<dyn SessionSource>, WatcherError> {
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(super::source_windows::WindowsSessionSource::new(config)?))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = config;
        Err(WatcherError::UnsupportedPlatform)
    }
}
