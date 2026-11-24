use anyhow::Result;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tokio::task::JoinHandle;

use crate::{
    config::{ConnectionMode, TcpKeepaliveConfig},
    streamer::{StreamerMsg, tcp_streamer},
};

use super::{
    AudioStream, ConnectError, StreamerTrait, TransportPath,
    adb_streamer::{
        self, DeviceKind, ensure_reverse_proxy, launch_android_app, probe_devices,
        remove_adb_reverse_proxy, reverse_mapping_present,
    },
    tcp_streamer::{TcpStreamer, TcpStreamerState},
};

/// How often the watcher re-reads `adb devices`. Kept at the same cadence as
/// the ADB watcher so both transports react to a plug within one tick.
const DEVICE_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Automatic transport: one wildcard listener that serves both paths.
///
/// `adb reverse tcp:<port> tcp:<port>` forwards to the PC's loopback and a
/// wildcard socket accepts loopback and LAN connections identically, so the
/// cable can come and go without swapping streamers, rebinding the port or
/// rebuilding the audio path. The watcher only owns the tunnel.
pub struct AutoStreamer {
    tcp_streamer: TcpStreamer,
    /// owns the `adb reverse` mapping for the lifetime of the streamer
    device_watch: JoinHandle<()>,
    /// LAN address the phone is told to use once the cable is out
    fallback_ip: Option<IpAddr>,
}

/// Log `note` only when it differs from the previous one, so a cable that stays
/// plugged in without authorization produces one line instead of one per tick.
fn note_once(last: &mut Option<String>, note: String) {
    if last.as_deref() != Some(note.as_str()) {
        warn!("{note}");
        *last = Some(note);
    }
}

/// Poll the wired device set and keep exactly one reverse tunnel alive.
/// Attach and detach are edges; a tunnel that vanished under a still-attached
/// device (`adb kill-server`, server restart, tunnel GC) is repaired on the
/// next tick because the mapping is verified, not just the device list.
fn spawn_device_watch(port: u16, fallback_ip: Option<IpAddr>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut attached: Vec<String> = Vec::new();
        // tracker per failure category: clearing one must not silence another
        let mut adb_note: Option<String> = None;
        let mut device_note: Option<String> = None;

        loop {
            let devices = match probe_devices().await {
                Ok(devices) => {
                    adb_note = None;
                    devices
                }
                Err(e) => {
                    // No `adb` binary or a dead server: stay in the listening
                    // state (AC7) and retry at the poll cadence.
                    note_once(&mut adb_note, format!("adb unavailable: {e}"));
                    attached.clear();
                    tokio::time::sleep(DEVICE_POLL_INTERVAL).await;
                    continue;
                }
            };

            let wired: Vec<String> = devices
                .iter()
                .filter(|device| device.kind == DeviceKind::Wired)
                .map(|device| device.serial.clone())
                .collect();
            let unauthorized: Vec<&str> = devices
                .iter()
                .filter(|device| device.kind == DeviceKind::Unauthorized)
                .map(|device| device.serial.as_str())
                .collect();

            match wired.first() {
                Some(serial) => {
                    if !attached.contains(serial) {
                        info!("wired device attached: {serial}");
                        match ensure_reverse_proxy(serial, port).await {
                            Ok(()) => {
                                device_note = None;
                                if let Err(e) =
                                    launch_android_app(serial, fallback_ip, port).await
                                {
                                    note_once(
                                        &mut device_note,
                                        format!("can't auto-start AndroidMic on {serial}: {e}"),
                                    );
                                }
                            }
                            Err(e) => note_once(
                                &mut device_note,
                                format!("can't create adb reverse proxy for {serial}: {e}"),
                            ),
                        }
                    } else {
                        match reverse_mapping_present(serial, port).await {
                            Ok(true) => device_note = None,
                            Ok(false) => {
                                info!("adb reverse tunnel missing for {serial}; rebuilding");
                                match ensure_reverse_proxy(serial, port).await {
                                    Ok(()) => {
                                        device_note = None;
                                        // The tunnel coming back does not mean the
                                        // phone is still streaming: an adb server
                                        // restart took the app down with it.
                                        if let Err(e) =
                                            launch_android_app(serial, fallback_ip, port).await
                                        {
                                            note_once(
                                                &mut device_note,
                                                format!(
                                                    "can't auto-start AndroidMic on {serial}: {e}"
                                                ),
                                            );
                                        }
                                    }
                                    Err(e) => note_once(
                                        &mut device_note,
                                        format!("can't rebuild adb reverse tunnel: {e}"),
                                    ),
                                }
                            }
                            Err(e) => note_once(
                                &mut device_note,
                                format!("can't read the adb reverse tunnel of {serial}: {e}"),
                            ),
                        }
                    }

                    if wired.len() > 1 {
                        note_once(
                            &mut device_note,
                            format!(
                                "{} wired devices attached; routing the stream over {serial} only",
                                wired.len()
                            ),
                        );
                    }
                }
                None => {
                    if !attached.is_empty() {
                        // The tunnel dies with the transport: nothing to clean.
                        // The phone keeps the connection over Wi-Fi from here.
                        info!("USB link lost; waiting for the phone over Wi-Fi");
                    }

                    if unauthorized.is_empty() {
                        device_note = None;
                    } else {
                        note_once(
                            &mut device_note,
                            format!(
                                "cable attached but debugging not authorized ({}); \
                                 falling back to Wi-Fi",
                                unauthorized.join(", ")
                            ),
                        );
                    }
                }
            }

            attached = wired;
            tokio::time::sleep(DEVICE_POLL_INTERVAL).await;
        }
    })
}

pub async fn new(
    port: u16,
    stream_config: AudioStream,
    keepalive: TcpKeepaliveConfig,
    on_demand_enabled: bool,
    fallback_ip: Option<IpAddr>,
) -> Result<AutoStreamer, ConnectError> {
    // Wildcard bind: serves the LAN like `Tcp` did, and loopback for the
    // `adb reverse` tunnel, on the same socket.
    let tcp_streamer = tcp_streamer::new(
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        port,
        stream_config,
        keepalive,
        on_demand_enabled,
    )
    .await?;
    let port = tcp_streamer.port;

    match fallback_ip {
        Some(ip) => info!("auto transport on port {port}, Wi-Fi fallback address {ip}"),
        None => warn!(
            "auto transport on port {port} without a LAN address: \
             the phone can only reach this PC over USB"
        ),
    }

    let device_watch = spawn_device_watch(port, fallback_ip);

    Ok(AutoStreamer {
        tcp_streamer,
        device_watch,
        fallback_ip,
    })
}

impl AutoStreamer {
    fn path(&self) -> Option<TransportPath> {
        self.tcp_streamer.peer_addr().map(|peer| {
            if peer.ip().is_loopback() {
                TransportPath::Wired
            } else {
                TransportPath::Lan(peer.ip())
            }
        })
    }
}

impl StreamerTrait for AutoStreamer {
    async fn next(&mut self) -> Result<Option<StreamerMsg>, ConnectError> {
        let was_streaming = matches!(&self.tcp_streamer.state, TcpStreamerState::Streaming { .. });
        let msg = self.tcp_streamer.next().await?;

        if was_streaming && matches!(msg, Some(StreamerMsg::Listening { .. })) {
            info!("connection dropped; the wildcard listener is back up, dial in over USB or Wi-Fi");
        }

        let path = self.path();
        Ok(msg.map(|msg| match msg {
            StreamerMsg::Connected { ip, port, .. } => StreamerMsg::Connected {
                ip,
                port,
                mode: ConnectionMode::Auto,
                path,
            },
            // The bind is wildcard, so the raw address is `0.0.0.0`; the
            // address worth logging is the one the phone dials next.
            StreamerMsg::Listening { port, .. } => StreamerMsg::Listening {
                ip: self.fallback_ip,
                port,
            },
            other => other,
        }))
    }

    fn reconfigure_stream(&mut self, config: AudioStream) {
        self.tcp_streamer.reconfigure_stream(config)
    }

    fn set_window_visible(&mut self, visible: bool) {
        self.tcp_streamer.set_window_visible(visible)
    }

    fn set_capture_demand(&mut self, enabled: bool) {
        self.tcp_streamer.set_capture_demand(enabled)
    }

    fn set_capture_degraded(&mut self, degraded: bool) {
        self.tcp_streamer.set_capture_degraded(degraded)
    }

    fn status(&self) -> StreamerMsg {
        match &self.tcp_streamer.state {
            TcpStreamerState::Listening { .. } => StreamerMsg::Listening {
                // The bind is wildcard; the address worth showing is the one
                // the phone dials when the cable is out.
                ip: self.fallback_ip,
                port: Some(self.tcp_streamer.port),
            },
            TcpStreamerState::Streaming { .. } => StreamerMsg::Connected {
                ip: self.tcp_streamer.peer_addr().map(|peer| peer.ip()),
                port: Some(self.tcp_streamer.port),
                mode: ConnectionMode::Auto,
                path: self.path(),
            },
        }
    }
}

impl Drop for AutoStreamer {
    fn drop(&mut self) {
        self.device_watch.abort();
        let port = self.tcp_streamer.port;
        tokio::spawn(async move {
            let devices = adb_streamer::get_wired_devices().await.unwrap_or_default();

            for device_id in devices {
                if let Err(e) = remove_adb_reverse_proxy(&device_id, port).await {
                    warn!("cannot remove adb proxy for device {device_id}: {e}");
                }
            }
        });
    }
}
