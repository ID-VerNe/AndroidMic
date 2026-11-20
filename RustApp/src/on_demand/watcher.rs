//! Watcher thread: polls the session source on a fixed interval and forwards
//! debounced demand (or a degradation reason) to the UI over a channel.
//!
//! The COM/source layer lives on this dedicated thread so its apartment state
//! is consistent; the UI side only sees plain [`WatcherUpdate`] values.

use std::panic::{self, AssertUnwindSafe};
use std::time::{Duration, Instant};

use cosmic::iced::futures::SinkExt;
use cosmic::iced::futures::Stream;
use cosmic::iced::stream;
use tokio::sync::{
    Mutex,
    mpsc::{self, UnboundedReceiver, UnboundedSender},
};
use tracing::warn;

use super::{Consumer, create_source, DemandMachine, OnDemandCaptureConfig, WatcherError};

/// How often the session source is polled.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Result of one watcher cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatcherUpdate {
    /// Debounced demand: true means a real client is recording, phone may capture.
    Demand(bool),
    /// Watcher fell back to always-on for the attached reason.
    Degraded(WatcherError),
}

/// Receiver half handed to the iced subscription, mirroring the tray stream.
#[derive(Clone)]
pub struct WatcherStream {
    receiver: std::sync::Arc<Mutex<UnboundedReceiver<WatcherUpdate>>>,
}

impl WatcherStream {
    pub fn sub(self) -> impl Stream<Item = WatcherUpdate> {
        let receiver = self.receiver.clone();

        stream::channel(1, |mut sender| async move {
            loop {
                let mut rx = receiver.lock().await;
                match rx.recv().await {
                    Some(update) => {
                        if sender.send(update).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        })
    }
}

/// Spawn the watcher thread. A source construction failure, a mid-run panic,
/// or a failed spawn is reported as a [`WatcherUpdate::Degraded`]; the caller
/// treats every one of them as always-on (fail-open).
pub fn spawn_watcher(config: &OnDemandCaptureConfig) -> WatcherStream {
    let (tx, rx) = mpsc::unbounded_channel();
    let config = config.clone();

    let tx_thread = tx.clone();
    let spawned = std::thread::Builder::new()
        .name("on-demand-watcher".into())
        .spawn(move || {
            // Catch any panic from the COM/source layer and report Degraded
            // instead of letting the thread die silently on the last state.
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                run_watcher(config, tx_thread.clone())
            }));
            if let Err(payload) = result {
                let reason = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned());
                match reason {
                    Some(reason) => {
                        warn!("on-demand watcher panicked: {reason}; forcing always-on")
                    }
                    None => warn!("on-demand watcher panicked; forcing always-on"),
                }
                let _ = tx_thread.send(WatcherUpdate::Degraded(WatcherError::ThreadPanicked));
            }
        });

    if spawned.is_err() {
        warn!("failed to spawn on-demand watcher thread; forcing always-on");
        let _ = tx.send(WatcherUpdate::Degraded(WatcherError::SpawnFailed));
    }

    WatcherStream {
        receiver: std::sync::Arc::new(Mutex::new(rx)),
    }
}

fn run_watcher(config: OnDemandCaptureConfig, tx: UnboundedSender<WatcherUpdate>) {
    let mut source = match create_source(&config) {
        Ok(source) => source,
        Err(error) => {
            let _ = tx.send(WatcherUpdate::Degraded(error));
            return;
        }
    };

    let mut machine = DemandMachine::new(std::process::id());
    let start = Instant::now();

    loop {
        let now = start.elapsed().as_millis() as u64;
        let consumers: Result<Vec<Consumer>, WatcherError> = source.snapshot();
        let update = match consumers {
            Ok(consumers) => WatcherUpdate::Demand(machine.update(&consumers, now)),
            Err(error) => WatcherUpdate::Degraded(error),
        };

        if tx.send(update).is_err() {
            break;
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}
