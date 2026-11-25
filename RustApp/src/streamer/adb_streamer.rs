use anyhow::Result;
use std::net::IpAddr;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::{
    config::{ConnectionMode, TcpKeepaliveConfig},
    streamer::{StreamerMsg, tcp_streamer},
};

use super::{
    AudioStream, ConnectError, StreamerTrait, TransportPath,
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
/// upper bound for one `adb` invocation. A server-starting call can block on a
/// pipe the daemon inherited, which is not a real answer to wait for.
const ADB_CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Android app package names. The debug build uses an `applicationIdSuffix`.
pub(crate) const ANDROID_APP_PACKAGES: [&str; 2] = [
    "io.github.teamclouday.AndroidMic",
    "io.github.teamclouday.AndroidMic.debug",
];
/// Extra read by MainActivity to start the stream without a phone-side tap.
const AUTO_CONNECT_EXTRA: &str = "io.github.teamclouday.androidMic.extra.AUTO_CONNECT";
/// Extra carrying the PC's LAN address, so the phone can fall back to Wi-Fi
/// after an unplug without the user typing an IP.
const PC_IP_EXTRA: &str = "io.github.teamclouday.androidMic.extra.PC_IP";
/// Extra carrying the PC's listening port.
const PC_PORT_EXTRA: &str = "io.github.teamclouday.androidMic.extra.PC_PORT";

/// What an `adb devices` line actually is. Only `Wired` may drive the loopback
/// tunnel of the automatic transport: a wireless-debugging serial is already a
/// network peer and an emulator is not a phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceKind {
    /// USB cable, debugging authorized.
    Wired,
    /// `adb connect host:port`.
    Wireless,
    /// `emulator-<n>`.
    Emulator,
    /// USB cable, debugging not authorized yet.
    Unauthorized,
    /// Any other state (`offline`, `no permissions`, ...).
    Unusable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdbDevice {
    pub serial: String,
    pub kind: DeviceKind,
}

/// A serial is wired when it carries no `host:port` suffix and is not an
/// emulator. `adb devices -l` no longer prints a `usb:` field on current
/// platform-tools (verified on 37.0.0 with a MI 9 attached), so the serial alone
/// has to be enough.
pub(crate) fn is_wired_serial(serial: &str) -> bool {
    !serial.contains(':') && !serial.starts_with("emulator-")
}

/// Parse `adb devices` output. Pure so the classification can be unit-tested
/// against captured platform-tools output.
pub(crate) fn classify_devices(output: &str) -> Vec<AdbDevice> {
    output
        .lines()
        .skip(1) // "List of devices attached"
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?;
            let state = parts.next()?;

            let kind = match state {
                "device" if serial.starts_with("emulator-") => DeviceKind::Emulator,
                "device" if serial.contains(':') => DeviceKind::Wireless,
                "device" => DeviceKind::Wired,
                "unauthorized" => DeviceKind::Unauthorized,
                _ => DeviceKind::Unusable,
            };

            Some(AdbDevice {
                serial: serial.to_string(),
                kind,
            })
        })
        .collect()
}

/// Whether `adb reverse --list` already maps `tcp:<port>` to `tcp:<port>`.
/// The listing looks like `UsbFfs tcp:54345 tcp:54345` (or `host-3 tcp:...`).
pub(crate) fn reverse_list_has_port(listing: &str, port: u16) -> bool {
    let mapping = format!("tcp:{port} tcp:{port}");
    listing.lines().any(|line| line.contains(&mapping))
}

pub(crate) async fn probe_devices() -> Result<Vec<AdbDevice>, ConnectError> {
    let mut cmd = Command::new("adb");
    cmd.arg("devices");

    let output = exec_cmd(cmd).await?;
    Ok(classify_devices(&output))
}

/// Every serial able to accept `adb reverse`. `ConnectionMode::Adb` deliberately
/// keeps emulators: a tunnel to an emulator is legitimate, and this PR must not
/// narrow a transport it does not own.
async fn get_connected_devices() -> Result<Vec<String>, ConnectError> {
    Ok(probe_devices()
        .await?
        .into_iter()
        .filter(|device| {
            matches!(
                device.kind,
                DeviceKind::Wired | DeviceKind::Wireless | DeviceKind::Emulator
            )
        })
        .map(|device| device.serial)
        .collect())
}

/// USB-attached, authorized devices only: the set the automatic transport may
/// route over the loopback tunnel.
pub(crate) async fn get_wired_devices() -> Result<Vec<String>, ConnectError> {
    Ok(probe_devices()
        .await?
        .into_iter()
        .filter(|device| device.kind == DeviceKind::Wired)
        .map(|device| device.serial)
        .collect())
}

/// Whether the reverse tunnel of `serial` is still alive. `adb kill-server`, an
/// adb server restart or a replug can drop it while the device stays attached.
pub(crate) async fn reverse_mapping_present(
    serial: &str,
    port: u16,
) -> Result<bool, ConnectError> {
    let mut cmd = Command::new("adb");
    cmd.arg("-s").arg(serial).arg("reverse").arg("--list");

    let listing = exec_cmd(cmd).await?;
    Ok(reverse_list_has_port(&listing, port))
}

pub(crate) async fn remove_adb_reverse_proxy(
    device_id: &str,
    port: u16,
) -> Result<(), ConnectError> {
    let mut cmd = Command::new("adb");
    cmd.arg("-s")
        .arg(device_id)
        .arg("reverse")
        .arg("--remove")
        .arg(format!("tcp:{}", port));

    exec_cmd(cmd).await?;

    Ok(())
}

pub(crate) async fn exec_cmd(mut cmd: Command) -> Result<String, ConnectError> {
    // https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x08000000);

    // `adb devices` starts the server on demand, and the daemon it spawns
    // inherits the stdout/stderr pipes that `output()` is reading. The client
    // exits, the daemon keeps the write ends open, and the read never sees EOF:
    // the caller blocks forever. Observed on a cold start (PC app launched
    // before any adb server existed), where the Auto watcher then never polled
    // again and the phone stayed on Wi-Fi. Bound the call so a cold start costs
    // one tick instead of the whole session.
    let output =
        match tokio::time::timeout(ADB_CALL_TIMEOUT, cmd.kill_on_drop(true).output()).await {
            Ok(result) => result.map_err(ConnectError::CommandFailed)?,
            Err(_) => {
                // Bring the daemon up detached so the retry talks to a live
                // server instead of spawning another one behind our pipes.
                start_adb_server().await;
                return Err(ConnectError::AdbTimeout(ADB_CALL_TIMEOUT));
            }
        };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        return Err(ConnectError::AdbStatusCommand {
            code: output.status.code(),
            stderr,
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(stdout)
}

/// Start the adb server without handing our pipes to the daemon.
///
/// Every `adb` call that finds no server spawns one, and that daemon inherits
/// whatever handles the child was given. Pointing all three streams at
/// `Stdio::null()` leaves nothing to inherit, so a cold start stays clean
/// instead of hanging the first `output()` (see `exec_cmd`).
pub async fn start_adb_server() {
    let mut cmd = Command::new("adb");
    cmd.arg("start-server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x08000000);

    // Same bound as any other call: the watcher calls this before its first
    // poll, so an adb server that never becomes ready must not park the loop
    // that is supposed to notice the device later.
    match tokio::time::timeout(ADB_CALL_TIMEOUT, cmd.kill_on_drop(true).status()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => debug!("can't start the adb server: {e}"),
        Err(_) => debug!("adb start-server did not finish within {ADB_CALL_TIMEOUT:?}"),
    }
}

/// Install (or refresh) the reverse tunnel of one device.
pub(crate) async fn ensure_reverse_proxy(device_id: &str, port: u16) -> Result<(), ConnectError> {
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

    Ok(())
}

/// Recreate the reverse proxy for every given device.
/// `new()` and the post-drop recovery path share this sequence.
async fn recreate_reverse_proxies(devices: &[String], port: u16) -> Result<(), ConnectError> {
    if devices.is_empty() {
        return Err(ConnectError::NoAdbDevice);
    }

    for device_id in devices {
        ensure_reverse_proxy(device_id, port).await?;
    }

    Ok(())
}

/// Rebuild the reverse proxy with a few bounded retries. A `adb kill-server`
/// or a replug can remove the tunnel while the local TCP listener stays alive.
async fn rebuild_reverse_proxies(port: u16) -> Result<(), ConnectError> {
    let mut last_error = None;

    for attempt in 0..ADB_REVERSE_ATTEMPTS {
        let result = match get_connected_devices().await {
            Ok(devices) => recreate_reverse_proxies(&devices, port).await,
            Err(e) => Err(e),
        };

        match result {
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

/// The package currently running on the phone, if either build is.
///
/// The release app and the debug build (`applicationIdSuffix`) can be installed
/// side by side. Launching the wrong one is silent: the intent lands on an app
/// that is not streaming, so the phone keeps its Wi-Fi connection instead of
/// moving back to the tunnel after the cable is plugged in again.
async fn running_package(device_id: &str) -> Option<&'static str> {
    for package in ANDROID_APP_PACKAGES {
        let mut cmd = Command::new("adb");
        cmd.arg("-s")
            .arg(device_id)
            .arg("shell")
            .arg("pidof")
            .arg(package);

        // `pidof` exits non-zero when nothing matches, so an error here just
        // means "not running"; a ROM without `pidof` falls back to the default
        // order instead of failing the launch.
        if exec_cmd(cmd).await.is_ok_and(|pids| !pids.is_empty()) {
            return Some(package);
        }
    }

    None
}

/// Ask the phone app to come to the foreground and auto-connect. This is
/// what removes the phone-side Connect tap when the app is not already open.
/// The PC's LAN address and port ride along so an `Auto` phone can fall back to
/// Wi-Fi without the user typing them. The running package goes first, the
/// release build before the debug one otherwise; a missing package or a MIUI
/// background-launch block only produces a warning.
pub(crate) async fn launch_android_app(
    device_id: &str,
    fallback_ip: Option<IpAddr>,
    port: u16,
) -> Result<(), ConnectError> {
    let mut last_error = None;

    // Dial the app that is actually on screen first; the fixed order only
    // decides which one to try when neither is running.
    let mut packages: Vec<&'static str> = running_package(device_id).await.into_iter().collect();
    for package in ANDROID_APP_PACKAGES {
        if !packages.contains(&package) {
            packages.push(package);
        }
    }

    for package in packages {
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
            .arg("true")
            .arg("--ei")
            .arg(PC_PORT_EXTRA)
            .arg(port.to_string());

        if let Some(ip) = fallback_ip {
            cmd.arg("--es").arg(PC_IP_EXTRA).arg(ip.to_string());
        }

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
                match recreate_reverse_proxies(&devices, port).await {
                    Ok(()) => {
                        present = true;
                        for device_id in &devices {
                            if let Err(e) = launch_android_app(device_id, None, port).await {
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

pub async fn new(
    port: u16,
    stream_config: AudioStream,
    keepalive: TcpKeepaliveConfig,
    on_demand_enabled: bool,
) -> Result<AdbStreamer, ConnectError> {
    let tcp_streamer = tcp_streamer::new(
        "127.0.0.1".parse().unwrap(),
        port,
        stream_config,
        keepalive,
        on_demand_enabled,
    )
    .await?;
    let port = tcp_streamer.port;

    let devices = get_connected_devices().await?;
    let present = !devices.is_empty();

    if present {
        recreate_reverse_proxies(&devices, port).await?;
        for device_id in &devices {
            if let Err(e) = launch_android_app(device_id, None, port).await {
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

        // ADB mode always rides the tunnel, so the live path is wired even
        // though the socket peer is loopback.
        Ok(msg.map(|msg| match msg {
            StreamerMsg::Connected { ip, port, mode, .. } => StreamerMsg::Connected {
                ip,
                port,
                mode,
                path: Some(TransportPath::Wired),
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
                ip: None,
                port: None,
            },
            TcpStreamerState::Streaming { .. } => StreamerMsg::Connected {
                ip: None,
                port: None,
                mode: ConnectionMode::Adb,
                path: Some(TransportPath::Wired),
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

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from platform-tools 37.0.0 on Windows with a MI 9 attached.
    const USB: &str = "List of devices attached\n\
9e779dea               device product:cepheus model:MI_9 device:cepheus transport_id:1\n";
    // `adb connect 192.168.1.5:5555`
    const WIRELESS: &str = "List of devices attached\n\
192.168.1.5:5555       device product:cepheus model:MI_9 device:cepheus transport_id:2\n";
    const EMULATOR: &str = "List of devices attached\n\
emulator-5554          device product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 transport_id:3\n";
    // Cable in, the RSA prompt not accepted yet.
    const UNAUTHORIZED: &str = "List of devices attached\n\
9e779dea               unauthorized usb:1-1 transport_id:2\n";
    const EMPTY: &str = "List of devices attached\n";

    fn kinds(output: &str) -> Vec<(String, DeviceKind)> {
        classify_devices(output)
            .into_iter()
            .map(|device| (device.serial, device.kind))
            .collect()
    }

    #[test]
    fn wired_usb_device_is_classified_as_wired() {
        assert_eq!(kinds(USB), vec![("9e779dea".into(), DeviceKind::Wired)]);
    }

    #[test]
    fn wireless_debugging_is_not_wired() {
        // A `host:port` serial is already a network peer: routing it over the
        // loopback tunnel would duplicate the path the phone already has.
        assert_eq!(
            kinds(WIRELESS),
            vec![("192.168.1.5:5555".into(), DeviceKind::Wireless)]
        );
        assert!(!is_wired_serial("192.168.1.5:5555"));
    }

    #[test]
    fn emulator_is_not_wired() {
        assert_eq!(
            kinds(EMULATOR),
            vec![("emulator-5554".into(), DeviceKind::Emulator)]
        );
        assert!(!is_wired_serial("emulator-5554"));
    }

    #[test]
    fn unauthorized_cable_is_not_wired() {
        // AC4: a cable with an unaccepted debugging prompt must fall back to
        // Wi-Fi instead of being counted as a usable loopback device.
        assert_eq!(
            kinds(UNAUTHORIZED),
            vec![("9e779dea".into(), DeviceKind::Unauthorized)]
        );
    }

    #[test]
    fn empty_listing_has_no_devices() {
        assert!(kinds(EMPTY).is_empty());
        assert!(kinds("").is_empty());
    }

    #[test]
    fn offline_device_is_unusable() {
        let output = "List of devices attached\n9e779dea               offline transport_id:4\n";
        assert_eq!(kinds(output), vec![("9e779dea".into(), DeviceKind::Unusable)]);
    }

    #[test]
    fn reverse_listing_detects_the_mapping() {
        // Captured from `adb -s 9e779dea reverse --list` on platform-tools 37.0.0.
        assert!(reverse_list_has_port("UsbFfs tcp:54345 tcp:54345", 54345));
        assert!(reverse_list_has_port("host-3 tcp:54345 tcp:54345", 54345));
        assert!(reverse_list_has_port(
            "UsbFfs tcp:1234 tcp:1234\nUsbFfs tcp:54345 tcp:54345",
            54345
        ));
    }

    #[test]
    fn reverse_listing_rejects_other_ports_and_empty() {
        assert!(!reverse_list_has_port("", 54345));
        assert!(!reverse_list_has_port("UsbFfs tcp:54346 tcp:54346", 54345));
        // A one-sided mapping is not a usable tunnel.
        assert!(!reverse_list_has_port("UsbFfs tcp:54345 tcp:1", 54345));
    }
}
