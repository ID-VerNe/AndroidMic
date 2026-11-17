use anyhow::Result;
use std::time::Duration;
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::{
    config::ConnectionMode,
    streamer::{StreamerMsg, tcp_streamer},
};

use super::{
    AudioStream, ConnectError, StreamerTrait,
    tcp_streamer::{TcpStreamer, TcpStreamerState},
};

pub struct AdbStreamer {
    tcp_streamer: TcpStreamer,
    /// polls `adb devices` for the lifetime of this streamer, so a device
    /// attached after startup gets its reverse proxy without another Connect
    device_watch: JoinHandle<()>,
}

/// how often the device watcher runs `adb devices`
const DEVICE_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// attempts to recreate the reverse proxy after the TCP listener came back
const ADB_REVERSE_ATTEMPTS: u32 = 3;
/// base delay between reverse proxy rebuild attempts
const ADB_REVERSE_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Android app package names. The debug build uses an `applicationIdSuffix`.
const ANDROID_APP_PACKAGES: [&str; 2] = [
    "io.github.teamclouday.AndroidMic",
    "io.github.teamclouday.AndroidMic.debug",
];
/// Extra read by MainActivity to start the stream without a phone-side tap.
const AUTO_CONNECT_EXTRA: &str = "io.github.teamclouday.androidMic.extra.AUTO_CONNECT";

async fn get_connected_devices() -> Result<Vec<String>, ConnectError> {
    let mut cmd = Command::new("adb");
    cmd.arg("devices");

    let output = exec_cmd(cmd).await?;
    let mut devices = Vec::new();

    // Skip the first line which is "List of devices attached"
    for line in output.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // only an authorized, ready device can accept `adb reverse`; skip
        // `unauthorized` / `offline` entries
        if parts.len() >= 2 && parts[1] == "device" {
            devices.push(parts[0].to_string());
        }
    }

    Ok(devices)
}

async fn remove_adb_reverse_proxy(device_id: &str, port: u16) -> Result<(), ConnectError> {
    let mut cmd = Command::new("adb");
    cmd.arg("-s")
        .arg(device_id)
        .arg("reverse")
        .arg("--remove")
        .arg(format!("tcp:{}", port));

    exec_cmd(cmd).await?;

    Ok(())
}

async fn exec_cmd(mut cmd: Command) -> Result<String, ConnectError> {
    // https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x08000000);

    let status = cmd.output().await.map_err(ConnectError::CommandFailed)?;

    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr).to_string();

        return Err(ConnectError::AdbStatusCommand {
            code: status.status.code(),
            stderr,
        });
    }
    let stdout = String::from_utf8_lossy(&status.stdout).trim().to_string();
    Ok(stdout)
}

/// Recreate the reverse proxy for every currently attached device.
/// `new()` and the post-drop recovery path share this sequence.
async fn recreate_reverse_proxies(port: u16) -> Result<(), ConnectError> {
    let devices = get_connected_devices().await?;
    if devices.is_empty() {
        return Err(ConnectError::NoAdbDevice);
    }

    for device_id in &devices {
        if let Err(e) = remove_adb_reverse_proxy(device_id, port).await
            && !e.to_string().contains("not found")
        {
            warn!("cannot remove adb proxy for device {device_id}: {e}");
        }

        let mut cmd = Command::new("adb");
        cmd.arg("-s")
            .arg(device_id)
            .arg("reverse")
            .arg(format!("tcp:{}", port))
            .arg(format!("tcp:{}", port));
        exec_cmd(cmd).await?;
    }

    Ok(())
}

/// Rebuild the reverse proxy with a few bounded retries. A `adb kill-server`
/// or a replug can remove the tunnel while the local TCP listener stays alive.
async fn rebuild_reverse_proxies(port: u16) -> Result<(), ConnectError> {
    let mut last_error = None;

    for attempt in 0..ADB_REVERSE_ATTEMPTS {
        match recreate_reverse_proxies(port).await {
            Ok(()) => {
                info!("adb reverse proxy rebuilt");
                return Ok(());
            }
            Err(e) => {
                warn!(
                    "can't rebuild adb reverse proxy (attempt {}/{}): {e}",
                    attempt + 1,
                    ADB_REVERSE_ATTEMPTS
                );
                last_error = Some(e);
                if attempt + 1 < ADB_REVERSE_ATTEMPTS {
                    tokio::time::sleep(ADB_REVERSE_RETRY_DELAY * (attempt + 1)).await;
                }
            }
        }
    }

    Err(last_error.unwrap_or(ConnectError::NoAdbDevice))
}

/// Ask the phone app to come to the foreground and auto-connect. This is
/// what removes the phone-side Connect tap when the app is not already open.
/// Release package first, debug build as a fallback; a missing package or a
/// MIUI background-launch block only produces a warning.
async fn launch_android_app(device_id: &str) -> Result<(), ConnectError> {
    let mut last_error = None;

    for package in ANDROID_APP_PACKAGES {
        let mut cmd = Command::new("adb");
        cmd.arg("-s")
            .arg(device_id)
            .arg("shell")
            .arg("am")
            .arg("start")
            .arg("-n")
            .arg(format!(
                "{}/io.github.teamclouday.androidMic.ui.MainActivity",
                package
            ))
            .arg("--ez")
            .arg(AUTO_CONNECT_EXTRA)
            .arg("true");

        match exec_cmd(cmd).await {
            Ok(_) => {
                info!("launched AndroidMic ({package}) on {device_id}");
                return Ok(());
            }
            Err(e) => {
                warn!("can't launch AndroidMic ({package}) on {device_id}: {e}");
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or(ConnectError::NoAdbDevice))
}

/// Poll `adb devices` for the lifetime of the streamer. The watcher is a
/// separate task so it never cancels the TCP accept/handshake path.
fn spawn_device_watch(port: u16, initially_present: bool) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut present = initially_present;

        loop {
            tokio::time::sleep(DEVICE_POLL_INTERVAL).await;

            let devices = match get_connected_devices().await {
                Ok(devices) => devices,
                Err(e) => {
                    warn!("adb device poll failed: {e}");
                    continue;
                }
            };

            let now_present = !devices.is_empty();

            if now_present && !present {
                info!("adb device attached: {devices:?}");
                match rebuild_reverse_proxies(port).await {
                    Ok(()) => {
                        present = true;
                        for device_id in &devices {
                            if let Err(e) = launch_android_app(device_id).await {
                                warn!("can't auto-start AndroidMic on {device_id}: {e}");
                            }
                        }
                    }
                    Err(e) => warn!("can't create adb reverse proxy: {e}"),
                }
            } else if !now_present && present {
                info!("adb device detached");
                present = false;
            }
        }
    })
}

pub async fn new(port: u16, stream_config: AudioStream) -> Result<AdbStreamer, ConnectError> {
    let tcp_streamer = tcp_streamer::new("127.0.0.1".parse().unwrap(), port, stream_config).await?;
    let port = tcp_streamer.port;

    let devices = get_connected_devices().await?;
    let present = !devices.is_empty();

    if present {
        recreate_reverse_proxies(port).await?;
        for device_id in &devices {
            if let Err(e) = launch_android_app(device_id).await {
                warn!("can't auto-start AndroidMic on {device_id}: {e}");
            }
        }
    } else {
        info!("no adb device at startup, waiting for one on port {port}");
    }

    let device_watch = spawn_device_watch(port, present);

    let streamer = AdbStreamer {
        tcp_streamer,
        device_watch,
    };
    Ok(streamer)
}

impl StreamerTrait for AdbStreamer {
    async fn next(&mut self) -> Result<Option<StreamerMsg>, ConnectError> {
        let was_streaming = matches!(&self.tcp_streamer.state, TcpStreamerState::Streaming { .. });
        let msg = self.tcp_streamer.next().await?;

        if was_streaming && matches!(msg, Some(StreamerMsg::Listening { .. })) {
            info!("rebuilding adb reverse proxy after the connection dropped");
            // The local listener is already back up. If the phone was unplugged
            // there is nothing to rebuild yet, so keep the `Listening` message
            // and let a later Connect (or the device watcher) restore the tunnel
            // instead of turning the drop into a streamer error.
            if let Err(e) = rebuild_reverse_proxies(self.tcp_streamer.port).await {
                warn!("can't rebuild adb reverse proxy after the connection dropped: {e}");
            }
        }

        Ok(msg)
    }

    fn reconfigure_stream(&mut self, config: AudioStream) {
        self.tcp_streamer.reconfigure_stream(config)
    }

    fn set_window_visible(&mut self, visible: bool) {
        self.tcp_streamer.set_window_visible(visible)
    }

    fn status(&self) -> StreamerMsg {
        match &self.tcp_streamer.state {
            TcpStreamerState::Listening { .. } => StreamerMsg::Listening {
                ip: None,
                port: None,
            },
            TcpStreamerState::Streaming { .. } => StreamerMsg::Connected {
                ip: None,
                port: None,
                mode: ConnectionMode::Adb,
            },
        }
    }
}

impl Drop for AdbStreamer {
    fn drop(&mut self) {
        self.device_watch.abort();
        let port = self.tcp_streamer.port;
        tokio::spawn(async move {
            let devices: Vec<String> = get_connected_devices().await.unwrap_or_default();

            for device_id in devices {
                if let Err(e) = remove_adb_reverse_proxy(&device_id, port).await {
                    warn!("cannot remove adb proxy for device {device_id}: {e}");
                }
            }
        });
    }
}
