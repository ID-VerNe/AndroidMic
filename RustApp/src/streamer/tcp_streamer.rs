use std::{
    io,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures::{SinkExt, StreamExt};
use prost::Message;
use socket2::{SockRef, TcpKeepalive};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpSocket, TcpStream},
};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::{
    audio::process::ProcessCache,
    config::{ConnectionMode, TcpKeepaliveConfig},
    streamer::{
        CaptureLinkStatus, CHECK_1, CHECK_2, StreamerMsg, WriteError,
        message::{
            CaptureControl, CaptureEnvelope, CaptureHello, CaptureHelloAck,
            capture_envelope::Payload,
        },
    },
};

use super::{AudioPacketMessage, AudioStream, ConnectError, StreamerTrait};

const MAX_WAIT_TIME: Duration = Duration::from_millis(1500);

/// On-demand capture protocol version spoken by this PC build.
const CAPTURE_PROTOCOL_VERSION: u32 = 1;
/// Capability bit0: on-demand capture.
const CAPTURE_CAPABILITIES_ON_DEMAND: u32 = 1;
/// How long the PC waits for a matching `CaptureHelloAck` before treating the
/// peer as a legacy always-on phone.
const ACK_TIMEOUT: Duration = Duration::from_millis(800);

const DISCONNECT_LOOP_DETECTER_MAX: u32 = 1000;

/// delay before the first rebind after a dropped connection
const REBIND_INITIAL_DELAY: Duration = Duration::from_millis(500);
/// maximum delay between two rebind attempts
const REBIND_MAX_DELAY: Duration = Duration::from_secs(5);
/// backoff multiplier applied after each failed rebind attempt
const REBIND_BACKOFF_FACTOR: u32 = 2;
/// number of retries after the first failed bind before surfacing the bind error
const REBIND_MAX_RETRIES: u32 = 6;

pub struct TcpStreamer {
    ip: IpAddr,
    pub port: u16,
    pub state: TcpStreamerState,
    stream_config: AudioStream,
    process_cache: ProcessCache,
    keepalive: TcpKeepaliveConfig,
    capture: CaptureChannel,
}

/// On-demand capture negotiation/control state carried on the TCP (or ADB-over
/// TCP) connection. UDP/USB transports never own one: the control channel
/// exists only here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CaptureChannel {
    enabled: bool,
    state: CaptureLinkStatus,
    nonce: u32,
    ack_deadline: Option<Instant>,
    /// Latest debounced demand from the PC watcher.
    demand: bool,
    /// Demand changed while linked and still needs to be flushed to the peer.
    demand_dirty: bool,
    /// The local probe source is unavailable: fail open by forcing capture on.
    /// While degraded, a `false` demand can never pull the phone back to muted.
    degraded: bool,
}

impl CaptureChannel {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            state: CaptureLinkStatus::Inactive,
            nonce: 0,
            ack_deadline: None,
            demand: false,
            demand_dirty: false,
            degraded: false,
        }
    }

    /// Record a debounced demand change. Only a linked channel marks the demand
    /// dirty so it can be flushed; before the ack arrives or on a legacy peer
    /// the demand is cached but never sent (fail-open). While degraded the
    /// forced-on state wins: a `false` demand cannot mute the phone.
    fn set_demand(&mut self, enabled: bool) {
        if self.degraded && !enabled {
            return;
        }
        let changed = self.demand != enabled;
        self.demand = enabled;

        if changed && self.state == CaptureLinkStatus::Linked {
            self.demand_dirty = true;
        }
    }

    /// Mark the local probe source degraded (or recover from it). Degradation
    /// forces capture on end-to-end: the Hello advertises `capture_enabled=true`
    /// and, once linked, a `Control(true)` is flushed.
    fn set_degraded(&mut self, degraded: bool) {
        self.degraded = degraded;
        if degraded {
            let changed = !self.demand;
            self.demand = true;
            if changed && self.state == CaptureLinkStatus::Linked {
                self.demand_dirty = true;
            }
        }
    }

    /// Demand advertised on the Hello and flushed on Control: degraded fails
    /// open to always-on.
    fn effective_demand(&self) -> bool {
        self.degraded || self.demand
    }

    /// Ack did not arrive within the timeout: treat the peer as legacy and stay
    /// always-on. The cached demand is kept, but no control frame is sent.
    fn ack_timed_out(&mut self) {
        self.state = CaptureLinkStatus::Legacy;
        self.ack_deadline = None;
    }
}

/// Whether an ack matches the negotiated protocol version and nonce. A mismatch
/// means the peer is legacy; the read loop falls back to always-on.
fn ack_matches(ack: &CaptureHelloAck, nonce: u32) -> bool {
    ack.protocol_version == CAPTURE_PROTOCOL_VERSION && ack.nonce == nonce
}

#[allow(clippy::large_enum_variant)]
pub enum TcpStreamerState {
    Listening {
        listener: TcpListener,
    },
    Streaming {
        framed: Framed<TcpStream, LengthDelimitedCodec>,
        disconnect_loop_detecter: u32,
    },
}

pub async fn new(
    ip: IpAddr,
    port: u16,
    stream_config: AudioStream,
    keepalive: TcpKeepaliveConfig,
    on_demand_enabled: bool,
) -> Result<TcpStreamer, ConnectError> {
    let listener = bind_reusable(ip, port).await?;

    let addr = TcpListener::local_addr(&listener).map_err(ConnectError::NoLocalAddress)?;

    info!(
        "tcp keepalive configured: time={}s interval={}s retries={}",
        keepalive.time_duration().as_secs(),
        keepalive.interval_duration().as_secs(),
        keepalive.retry_count()
    );

    let streamer = TcpStreamer {
        ip,
        port: addr.port(),
        stream_config,
        state: TcpStreamerState::Listening { listener },
        process_cache: ProcessCache::new(),
        keepalive,
        capture: CaptureChannel::new(on_demand_enabled),
    };

    Ok(streamer)
}

/// Bind a listening socket with `SO_REUSEADDR` set, so a quick rebind after a
/// dropped connection does not fail while the previous socket is in `TIME_WAIT`.
async fn bind_reusable(ip: IpAddr, port: u16) -> Result<TcpListener, ConnectError> {
    let socket = match ip {
        IpAddr::V4(_) => TcpSocket::new_v4(),
        IpAddr::V6(_) => TcpSocket::new_v6(),
    }
    .map_err(|e| ConnectError::CantBindPort(port, e))?;

    socket
        .set_reuseaddr(true)
        .map_err(|e| ConnectError::CantBindPort(port, e))?;
    socket
        .bind(SocketAddr::from((ip, port)))
        .map_err(|e| ConnectError::CantBindPort(port, e))?;
    socket
        .listen(128)
        .map_err(|e| ConnectError::CantBindPort(port, e))
}

impl StreamerTrait for TcpStreamer {
    fn reconfigure_stream(&mut self, stream_config: AudioStream) {
        self.stream_config = stream_config;
        self.process_cache.clear();
    }

    fn set_capture_demand(&mut self, enabled: bool) {
        self.capture.set_demand(enabled);
    }

    fn set_capture_degraded(&mut self, degraded: bool) {
        self.capture.set_degraded(degraded);
    }

    fn set_window_visible(&mut self, visible: bool) {
        if self.stream_config.is_window_visible != visible {
            info!("window visibility changed: {visible}");
            self.stream_config.is_window_visible = visible;
        }
    }

    fn status(&self) -> StreamerMsg {
        match &self.state {
            TcpStreamerState::Listening { .. } => StreamerMsg::Listening {
                ip: Some(self.ip),
                port: Some(self.port),
            },
            TcpStreamerState::Streaming { .. } => StreamerMsg::Connected {
                ip: Some(self.ip),
                port: Some(self.port),
                mode: ConnectionMode::Tcp,
            },
        }
    }

    async fn next(&mut self) -> Result<Option<StreamerMsg>, ConnectError> {
        let outcome = match &mut self.state {
            TcpStreamerState::Listening { listener } => {
                let addr =
                    TcpListener::local_addr(listener).map_err(ConnectError::NoLocalAddress)?;

                info!("TCP server listening on {}", addr);

                let (mut stream, addr) =
                    listener.accept().await.map_err(ConnectError::CantAccept)?;

                let mut buf1 = [0u8; CHECK_1.len()];

                stream
                    .read_exact(&mut buf1)
                    .await
                    .map_err(|e| ConnectError::HandShakeFailed("reading", e))?;

                if buf1 != CHECK_1.as_bytes() {
                    let s = String::from_utf8_lossy(&buf1);

                    return Err(ConnectError::HandShakeFailed2(format!(
                        "{} != {}",
                        CHECK_1, s
                    )));
                }

                stream
                    .write_all(CHECK_2.as_bytes())
                    .await
                    .map_err(|e| ConnectError::HandShakeFailed("writing", e))?;

                // Enable TCP keepalive so a peer that vanishes without sending
                // FIN (e.g. Wi-Fi turned off) is detected instead of leaving the
                // read blocked on a half-open socket. Keepalive is an optional
                // hardening: if the platform rejects the option, warn and keep
                // the connection going instead of turning a best-effort socket
                // option into a fatal handshake failure.
                let keepalive = TcpKeepalive::new()
                    .with_time(self.keepalive.time_duration())
                    .with_interval(self.keepalive.interval_duration())
                    .with_retries(self.keepalive.retry_count());
                if let Err(e) = SockRef::from(&stream).set_tcp_keepalive(&keepalive) {
                    warn!(
                        "failed to enable TCP keepalive ({e}); continuing without it, \
                         half-open detection relies on EOF/IO errors"
                    );
                }

                info!("connection accepted, remote address: {}", addr);

                let mut framed = Framed::new(stream, LengthDelimitedCodec::new());

                // On-demand handshake: only the TCP/ADB control channel sends
                // Capture messages. If the feature is off we stay silent here
                // and behave exactly like the legacy build.
                if self.capture.enabled {
                    self.capture.nonce = new_nonce();
                    self.capture.state = CaptureLinkStatus::Negotiating;
                    self.capture.ack_deadline = Some(Instant::now() + ACK_TIMEOUT);

                    let hello = CaptureHello {
                        protocol_version: CAPTURE_PROTOCOL_VERSION,
                        capabilities: CAPTURE_CAPABILITIES_ON_DEMAND,
                        nonce: self.capture.nonce,
                        capture_enabled: self.capture.effective_demand(),
                    };
                    let envelope = CaptureEnvelope {
                        payload: Some(Payload::Hello(hello)),
                    };

                    let payload = prost::bytes::Bytes::from(envelope.encode_to_vec());
                    if let Err(e) = framed.send(payload).await {
                        // Fail open: a hello write failure must not kill the audio
                        // connection; the peer simply stays on always-on capture.
                        warn!("failed to send CaptureHello: {e}; staying always-on");
                        self.capture.state = CaptureLinkStatus::Legacy;
                    } else {
                        info!(
                            "CaptureHello sent (nonce={}), awaiting ack within {:?}",
                            self.capture.nonce, ACK_TIMEOUT
                        );
                    }
                }

                self.state = TcpStreamerState::Streaming {
                    framed,
                    disconnect_loop_detecter: 0,
                };

                Outcome::Msg(Some(StreamerMsg::Connected {
                    ip: Some(self.ip),
                    port: Some(self.port),
                    mode: ConnectionMode::Tcp,
                }))
            }
            TcpStreamerState::Streaming {
                framed,
                disconnect_loop_detecter: _,
            } => {
                // Flush a debounced demand change once the control channel is
                // linked. Control frames are only ever written here, on TCP.
                if self.capture.demand_dirty
                    && self.capture.state == CaptureLinkStatus::Linked
                {
                    self.capture.demand_dirty = false;
                    let control = CaptureControl {
                        enabled: self.capture.demand,
                    };
                    let envelope = CaptureEnvelope {
                        payload: Some(Payload::Control(control)),
                    };
                    let payload = prost::bytes::Bytes::from(envelope.encode_to_vec());
                    if let Err(e) = framed.send(payload).await {
                        // Fail open: a control write failure keeps the previous
                        // capture state instead of dropping the connection.
                        warn!("failed to send CaptureControl: {e}; keeping previous state");
                    } else {
                        debug!("CaptureControl enabled={}", self.capture.demand);
                    }
                }

                // While negotiating, race the read against the ack timeout: a
                // legacy peer that never answers degrades to always-on rather
                // than blocking the read loop forever.
                let frame_result = if self.capture.state == CaptureLinkStatus::Negotiating
                {
                    match tokio::time::timeout(ACK_TIMEOUT, framed.next()).await {
                        Ok(inner) => inner,
                        Err(_) => {
                            info!(
                                "no CaptureHelloAck within {ACK_TIMEOUT:?}; \
                                 legacy phone, fail-open always-on"
                            );
                            self.capture.ack_timed_out();
                            return Ok(Some(StreamerMsg::CaptureLink(
                                CaptureLinkStatus::Legacy,
                            )));
                        }
                    }
                } else {
                    framed.next().await
                };

                match frame_result {
                    Some(Ok(frame)) => {
                        // A frame that decodes as a CaptureHelloAck is consumed in
                        // every state. A mismatched or late ack must never reach
                        // the audio decoder: the ack's varint fields clash with the
                        // audio packet's length-delimited buffer, and a decoder
                        // mismatch would surface as a Fatal deserializer error
                        // that drops the stream.
                        let mut ack_consumed = false;
                        if let Ok(ack) = CaptureHelloAck::decode(&frame[..]) {
                            match self.capture.state {
                                CaptureLinkStatus::Negotiating
                                    if ack_matches(&ack, self.capture.nonce) =>
                                {
                                    info!(
                                        "CaptureHelloAck matched (nonce={}), on-demand linked",
                                        ack.nonce
                                    );
                                    self.capture.state = CaptureLinkStatus::Linked;
                                    self.capture.ack_deadline = None;
                                    // Re-emit the current demand now that the
                                    // control channel is live.
                                    self.capture.demand_dirty = true;
                                    return Ok(Some(StreamerMsg::CaptureLink(
                                        CaptureLinkStatus::Linked,
                                    )));
                                }
                                CaptureLinkStatus::Negotiating => {
                                    warn!(
                                        "CaptureHelloAck nonce/version mismatch (got {}, expected {}); \
                                         legacy, fail-open",
                                        ack.nonce, self.capture.nonce
                                    );
                                    self.capture.state = CaptureLinkStatus::Legacy;
                                }
                                other => {
                                    warn!(
                                        "late/stray CaptureHelloAck ignored (state={other:?}, ack nonce={})",
                                        ack.nonce
                                    );
                                }
                            }
                            ack_consumed = true;
                        }

                        if ack_consumed {
                            Outcome::Msg(None)
                        } else {
                        match AudioPacketMessage::decode(&frame[..]) {
                            Ok(packet) => {
                                let buffer_size = packet.buffer.len();
                                let sample_rate = packet.sample_rate;

                                match self
                                    .stream_config
                                    .process_audio_packet(packet, &mut self.process_cache)
                                {
                                    Ok(Some(buffer)) => {
                                        debug!("received {} bytes", buffer_size);
                                        Outcome::Msg(Some(StreamerMsg::UpdateAudioWave {
                                            data: AudioPacketMessage::to_wave_data(
                                                &buffer,
                                                sample_rate,
                                            ),
                                        }))
                                    }
                                    _ => Outcome::Msg(None),
                                }
                            }
                            Err(e) => Outcome::Fatal(ConnectError::WriteError(
                                WriteError::Deserializer(e),
                            )),
                        }
                        }
                    }

                    Some(Err(e)) => match e.kind() {
                        io::ErrorKind::TimedOut => Outcome::Msg(None), // timeout use to check for input on stdin
                        io::ErrorKind::WouldBlock => Outcome::Msg(None), // trigger on Linux when there is no stream input
                        _ => {
                            warn!("stream error: {e}, going back to listening");
                            Outcome::Drop
                        }
                    },
                    None => Outcome::Drop,
                }
            }
        };

        match outcome {
            Outcome::Msg(msg) => Ok(msg),
            Outcome::Drop => self.relisten().await,
            Outcome::Fatal(e) => Err(e),
        }
    }
}

impl TcpStreamer {
    /// Drop the active connection and rebind the same port, going back to
    /// `Listening` with a bounded exponential backoff. Used after a recoverable
    /// drop so another phone can reconnect without a manual Connect.
    async fn relisten(&mut self) -> Result<Option<StreamerMsg>, ConnectError> {
        info!(
            "connection closed, waiting to listen again on {}",
            self.port
        );

        let mut attempts = 0u32;
        loop {
            match bind_reusable(self.ip, self.port).await {
                Ok(listener) => {
                    self.process_cache.clear();
                    self.state = TcpStreamerState::Listening { listener };

                    return Ok(Some(StreamerMsg::Listening {
                        ip: Some(self.ip),
                        port: Some(self.port),
                    }));
                }
                Err(e) => {
                    let delay = rebind_delay(attempts);

                    if attempts >= REBIND_MAX_RETRIES {
                        error!(
                            "can't rebind {}:{} after {} attempts: {e}",
                            self.ip,
                            self.port,
                            attempts + 1
                        );
                        return Err(e);
                    }

                    warn!(
                        "can't rebind {}:{}: {e}; retry {}/{} in {:?}",
                        self.ip,
                        self.port,
                        attempts + 1,
                        REBIND_MAX_RETRIES,
                        delay
                    );
                    tokio::time::sleep(delay).await;
                    attempts += 1;
                }
            }
        }
    }
}

/// Generate a fresh handshake nonce. `rand` is a dev-dependency only, so seed
/// from the wall clock; the nonce only needs to be unique per connection.
fn new_nonce() -> u32 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    elapsed.as_secs() as u32 ^ elapsed.subsec_nanos()
}

/// Exponential backoff between rebind attempts: 500ms, 1s, 2s, 4s, capped at 5s.
fn rebind_delay(attempt: u32) -> Duration {
    let mut delay = REBIND_INITIAL_DELAY;
    for _ in 0..attempt {
        delay = delay
            .saturating_mul(REBIND_BACKOFF_FACTOR)
            .min(REBIND_MAX_DELAY);
    }
    delay
}

enum Outcome {
    Msg(Option<StreamerMsg>),
    /// the connection dropped; rebind the port and go back to listening
    Drop,
    /// unrecoverable protocol/device error, propagate to the runner
    Fatal(ConnectError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_timeout_is_800ms() {
        // The PC waits 800ms for a matching ack; anything slower is treated as
        // a legacy peer and fails open to always-on capture.
        assert_eq!(ACK_TIMEOUT, Duration::from_millis(800));
    }

    #[test]
    fn ack_matches_version_and_nonce() {
        let ack = CaptureHelloAck {
            protocol_version: CAPTURE_PROTOCOL_VERSION,
            capabilities: CAPTURE_CAPABILITIES_ON_DEMAND,
            nonce: 42,
        };

        assert!(ack_matches(&ack, 42));
        // Wrong nonce: a stale/echoed ack must not link the channel.
        assert!(!ack_matches(&ack, 43));

        let wrong_version = CaptureHelloAck {
            protocol_version: 9,
            capabilities: CAPTURE_CAPABILITIES_ON_DEMAND,
            nonce: 42,
        };
        assert!(!ack_matches(&wrong_version, 42));
    }

    #[test]
    fn disabled_channel_stays_inactive_and_ignores_demand() {
        let mut channel = CaptureChannel::new(false);
        assert_eq!(channel.state, CaptureLinkStatus::Inactive);

        channel.set_demand(true);
        // A disabled feature never marks a control frame dirty.
        assert!(!channel.demand_dirty);
    }

    #[test]
    fn demand_before_link_is_not_flushed() {
        let mut channel = CaptureChannel::new(true);
        channel.state = CaptureLinkStatus::Negotiating;

        channel.set_demand(true);
        assert!(!channel.demand_dirty);
    }

    #[test]
    fn ack_timeout_falls_back_to_legacy_fail_open() {
        let mut channel = CaptureChannel::new(true);
        channel.state = CaptureLinkStatus::Negotiating;
        channel.ack_deadline = Some(Instant::now() + ACK_TIMEOUT);
        channel.demand = true;

        channel.ack_timed_out();

        assert_eq!(channel.state, CaptureLinkStatus::Legacy);
        assert!(channel.ack_deadline.is_none());
        // Cached demand survives, but must not produce a control frame.
        assert!(!channel.demand_dirty);
    }

    #[test]
    fn legacy_channel_keeps_demand_but_never_sends() {
        let mut channel = CaptureChannel::new(true);
        channel.state = CaptureLinkStatus::Legacy;

        channel.set_demand(true);
        // Legacy peer: the cached demand must not produce a control frame.
        assert!(!channel.demand_dirty);
    }

    #[test]
    fn linked_channel_marks_demand_dirty() {
        let mut channel = CaptureChannel::new(true);
        channel.state = CaptureLinkStatus::Linked;

        channel.set_demand(true);
        assert!(channel.demand_dirty);
    }

    #[test]
    fn degraded_forces_always_on_before_and_after_link() {
        // Probe source unavailable / endpoint missing / COM failure / non-Windows
        // all surface as Degraded; the phone-facing demand must be on in every
        // state, both before the Hello and after linking.
        let mut before_link = CaptureChannel::new(true);
        before_link.state = CaptureLinkStatus::Negotiating;

        before_link.set_degraded(true);
        assert!(before_link.effective_demand());
        assert!(before_link.demand);
        // Not linked yet: nothing to flush, but the cached demand is on.
        assert!(!before_link.demand_dirty);

        let mut linked = CaptureChannel::new(true);
        linked.state = CaptureLinkStatus::Linked;
        linked.set_demand(false);

        linked.set_degraded(true);
        // Degradation while linked flushes Control(true).
        assert!(linked.demand_dirty);
        assert!(linked.effective_demand());
    }

    #[test]
    fn degraded_ignores_demand_false_so_phone_is_never_muted() {
        let mut channel = CaptureChannel::new(true);
        channel.state = CaptureLinkStatus::Linked;
        channel.set_degraded(true);
        channel.demand_dirty = false;

        // A late/queued false demand must not pull the phone back to muted
        // while the source is degraded.
        channel.set_demand(false);
        assert!(channel.demand, "degraded demand stays on");
        assert!(channel.effective_demand());
        assert!(!channel.demand_dirty);
    }

    #[test]
    fn recovery_clears_degradation_and_allows_demand_false_again() {
        let mut channel = CaptureChannel::new(true);
        channel.state = CaptureLinkStatus::Linked;
        channel.set_degraded(true);

        // A later healthy snapshot clears degradation; normal demand resumes.
        channel.set_degraded(false);
        channel.set_demand(false);
        assert!(!channel.degraded);
        assert!(!channel.demand);
        assert!(!channel.effective_demand());
        assert!(channel.demand_dirty);
    }

    #[test]
    fn degraded_demand_overrides_cached_off_demand_on_hello() {
        // The Hello advertises the effective demand: a degraded session must
        // send capture_enabled=true even if the last cached demand was off.
        let mut channel = CaptureChannel::new(true);
        channel.set_demand(false);
        assert!(!channel.effective_demand());

        channel.set_degraded(true);
        assert!(channel.effective_demand());
    }

    #[test]
    fn rebind_delay_grows_then_caps() {
        let cases = [
            (0, 500),
            (1, 1000),
            (2, 2000),
            (3, 4000),
            (4, 5000),
            (5, 5000),
            (6, 5000),
            (7, 5000),
        ];

        for (attempt, expected_ms) in cases {
            assert_eq!(
                rebind_delay(attempt),
                Duration::from_millis(expected_ms),
                "attempt {attempt}"
            );
        }
    }
}
