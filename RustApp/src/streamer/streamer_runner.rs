use cosmic::iced::futures::SinkExt;
use cosmic::iced::stream;
use either::Either;
use futures::Stream;
use futures::{
    future::{self},
    pin_mut,
};
use std::fmt::Debug;
use std::net::IpAddr;
use tokio::sync::mpsc::{self, Sender};

use crate::audio::{AudioProcessParams, playout::PlayoutWriter};
use crate::config::{ConnectionMode, TcpKeepaliveConfig};
use crate::streamer::{StreamerTrait, WriteError};

use super::{AudioStream, ConnectError, DummyStreamer, Streamer, tcp_streamer, udp_streamer};

#[derive(Debug)]
pub enum ConnectOption {
    Tcp {
        ip: IpAddr,
        port: u16,
        keepalive: TcpKeepaliveConfig,
        on_demand_enabled: bool,
    },
    Udp {
        ip: IpAddr,
        port: u16,
    },
    #[cfg(feature = "adb")]
    Adb {
        port: u16,
        keepalive: TcpKeepaliveConfig,
        on_demand_enabled: bool,
    },
    #[cfg(feature = "usb")]
    Usb,
}

/// On-demand capture negotiation state reported by the transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureLinkStatus {
    /// Feature disabled, or a transport that cannot carry the control channel:
    /// no `CaptureHello` is sent.
    Inactive,
    /// `CaptureHello` sent, awaiting a matching `CaptureHelloAck`.
    Negotiating,
    /// Ack matched the nonce: on-demand capture is linked.
    Linked,
    /// No matching ack within the timeout: legacy phone, stay always-on.
    Legacy,
}

/// Latest on-demand capture demand/degradation, cached in the runner.
///
/// A one-shot Degraded (source construction failure, non-Windows, spawn
/// failure, panic) or a demand update can arrive before the first Connect,
/// while the active streamer is the `DummyStreamer` (whose hooks are no-ops).
/// The runner keeps the latest state here and replays it onto every fresh
/// streamer so a new channel starts in the correct fail-open state instead of
/// `degraded=false, demand=false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CaptureDemandState {
    degraded: bool,
    demand: bool,
}

impl CaptureDemandState {
    fn record_demand(&mut self, enabled: bool) {
        self.demand = enabled;
    }

    fn record_degraded(&mut self, degraded: bool) {
        self.degraded = degraded;
    }

    /// Replay the cached state onto a fresh streamer: degradation first, so a
    /// forced-on state wins, then the cached demand.
    fn replay_on(&self, streamer: &mut impl StreamerTrait) {
        streamer.set_capture_degraded(self.degraded);
        streamer.set_capture_demand(self.demand);
    }
}

/// App -> Streamer
pub enum StreamerCommand {
    Connect {
        connect_options: ConnectOption,
        buff: PlayoutWriter,
        audio_params: AudioProcessParams,
        is_window_visible: bool,
    },
    ReconfigureStream {
        buff: PlayoutWriter,
        audio_params: AudioProcessParams,
        is_window_visible: bool,
    },
    SetWindowVisible(bool),
    /// Debounced on-demand demand from the PC session watcher. Only honoured
    /// once the control channel is linked; otherwise ignored (fail-open).
    SetCaptureDemand(bool),
    /// Local probe source degraded/recovered. Degradation forces capture on.
    SetCaptureDegraded(bool),
    Stop,
}

impl Debug for StreamerCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect {
                connect_options,
                buff: _,
                audio_params,
                is_window_visible,
            } => f
                .debug_struct("Connect")
                .field("connect_options", connect_options)
                .field("audio_params", audio_params)
                .field("is_window_visible", is_window_visible)
                .finish(),
            Self::ReconfigureStream {
                buff: _,
                audio_params,
                is_window_visible,
            } => f
                .debug_struct("ReconfigureStream")
                .field("audio_params", audio_params)
                .field("is_window_visible", is_window_visible)
                .finish(),
            Self::Stop => write!(f, "Stop"),
            Self::SetWindowVisible(visible) => write!(f, "SetWindowVisible({visible})"),
            Self::SetCaptureDemand(enabled) => write!(f, "SetCaptureDemand({enabled})"),
            Self::SetCaptureDegraded(degraded) => write!(f, "SetCaptureDegraded({degraded})"),
        }
    }
}

/// Streamer -> App
#[derive(Debug, Clone)]
pub enum StreamerMsg {
    UpdateAudioWave {
        data: Vec<(f32, f32)>,
    },
    Error(String),
    Listening {
        ip: Option<IpAddr>,
        port: Option<u16>,
    },
    Connected {
        ip: Option<IpAddr>,
        port: Option<u16>,
        mode: ConnectionMode,
    },
    Ready(Sender<StreamerCommand>),
    /// On-demand capture negotiation result for the current connection.
    CaptureLink(CaptureLinkStatus),
}

impl StreamerMsg {
    fn is_error(&self) -> bool {
        matches!(self, StreamerMsg::Error(..))
    }
}

async fn send(sender: &mut futures::channel::mpsc::Sender<StreamerMsg>, msg: StreamerMsg) {
    // never wait on the UI for audio wave updates, it would hold back the network reads
    if matches!(msg, StreamerMsg::UpdateAudioWave { .. }) {
        let _ = sender.try_send(msg);
        return;
    }
    sender.send(msg).await.unwrap();
}

pub fn sub() -> impl Stream<Item = StreamerMsg> {
    stream::channel(5, |mut sender| async move {
        let (command_sender, mut command_receiver) = mpsc::channel(100);

        let mut streamer: Streamer = DummyStreamer::new();
        // Latest on-demand state, retained across streamer swaps and reconnects.
        let mut capture_state = CaptureDemandState::default();

        send(&mut sender, StreamerMsg::Ready(command_sender)).await;

        loop {
            let either = {
                let recv_future = command_receiver.recv();
                let process_future = streamer.next();

                pin_mut!(recv_future);
                pin_mut!(process_future);

                // This map it to remove the weird lifetime of future::Either
                match future::select(recv_future, process_future).await {
                    future::Either::Left((res, _)) => Either::Left(res),
                    future::Either::Right((res, _)) => Either::Right(res),
                }
            };

            match either {
                Either::Left(command) => {
                    if let Some(command) = command {
                        info!("received command {command:?}");
                        match command {
                            StreamerCommand::Connect {
                                connect_options,
                                buff,
                                audio_params,
                                is_window_visible,
                            } => {
                                let stream_config =
                                    AudioStream::new(buff, audio_params, is_window_visible);
                                let new_streamer: Result<Streamer, ConnectError> =
                                    match connect_options {
                                        ConnectOption::Tcp {
                                            ip,
                                            port,
                                            keepalive,
                                            on_demand_enabled,
                                        } => tcp_streamer::new(
                                            ip,
                                            port,
                                            stream_config,
                                            keepalive,
                                            on_demand_enabled,
                                        )
                                        .await
                                        .map(Streamer::from),
                                        #[cfg(feature = "adb")]
                                        ConnectOption::Adb {
                                            port,
                                            keepalive,
                                            on_demand_enabled,
                                        } => {
                                            crate::streamer::adb_streamer::new(
                                                port,
                                                stream_config,
                                                keepalive,
                                                on_demand_enabled,
                                            )
                                            .await
                                            .map(Streamer::from)
                                        }
                                        ConnectOption::Udp { ip, port } => {
                                            udp_streamer::new(ip, port, stream_config)
                                                .await
                                                .map(Streamer::from)
                                        }
                                        #[cfg(feature = "usb")]
                                        ConnectOption::Usb => {
                                            crate::streamer::usb_streamer::new(stream_config)
                                                .await
                                                .map(Streamer::from)
                                        }
                                    };

                                match new_streamer {
                                    Ok(mut new_streamer) => {
                                        send(&mut sender, new_streamer.status()).await;
                                        drop(streamer);
                                        // Replay the cached on-demand state onto the
                                        // fresh streamer: a one-shot Degraded that
                                        // arrived before this Connect (or while the
                                        // streamer was a Dummy) must reach the new
                                        // channel so its Hello advertises always-on.
                                        capture_state.replay_on(&mut new_streamer);
                                        streamer = new_streamer;
                                    }
                                    Err(e) => {
                                        error!("{e}");
                                        send(&mut sender, StreamerMsg::Error(e.to_string())).await;
                                    }
                                }
                            }
                            StreamerCommand::ReconfigureStream {
                                buff,
                                audio_params,
                                is_window_visible,
                            } => {
                                let stream_config =
                                    AudioStream::new(buff, audio_params, is_window_visible);

                                streamer.reconfigure_stream(stream_config);
                            }
                            StreamerCommand::Stop => {
                                drop(streamer);
                                streamer = DummyStreamer::new();
                            }
                            StreamerCommand::SetWindowVisible(visible) => {
                                streamer.set_window_visible(visible);
                            }
                            StreamerCommand::SetCaptureDemand(enabled) => {
                                capture_state.record_demand(enabled);
                                streamer.set_capture_demand(enabled);
                            }
                            StreamerCommand::SetCaptureDegraded(degraded) => {
                                capture_state.record_degraded(degraded);
                                streamer.set_capture_degraded(degraded);
                            }
                        }
                    }
                }
                Either::Right(res) => match res {
                    Ok(status) => {
                        if let Some(status) = status {
                            send(&mut sender, status).await;
                        }
                    }
                    Err(connect_error) => {
                        error!("{connect_error}");

                        if !matches!(
                            connect_error,
                            ConnectError::WriteError(WriteError::BufferOverfilled(..))
                        ) {
                            send(&mut sender, StreamerMsg::Error(connect_error.to_string())).await;
                            streamer = DummyStreamer::new();
                        }
                    }
                },
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal streamer double recording only the on-demand hooks; every other
    /// trait method is unreachable in these unit tests.
    struct FakeStreamer {
        degraded: Option<bool>,
        demand: Option<bool>,
    }

    impl StreamerTrait for FakeStreamer {
        async fn next(&mut self) -> Result<Option<StreamerMsg>, ConnectError> {
            std::future::pending::<()>().await;
            unreachable!()
        }

        fn reconfigure_stream(&mut self, _config: AudioStream) {}

        fn set_window_visible(&mut self, _visible: bool) {}

        fn status(&self) -> StreamerMsg {
            unreachable!()
        }

        fn set_capture_demand(&mut self, enabled: bool) {
            self.demand = Some(enabled);
        }

        fn set_capture_degraded(&mut self, degraded: bool) {
            self.degraded = Some(degraded);
        }
    }

    #[test]
    fn cached_degraded_state_is_replayed_to_a_new_streamer() {
        // One-shot Degraded before Connect (source failure / non-Windows / spawn
        // failure / panic, watcher thread exits): the runner cached degraded=true
        // with the stale cached demand=false, then replays onto the fresh streamer.
        let mut state = CaptureDemandState::default();
        state.record_degraded(true);
        state.record_demand(false);

        let mut fake = FakeStreamer {
            degraded: None,
            demand: None,
        };
        state.replay_on(&mut fake);

        assert_eq!(fake.degraded, Some(true));
        assert_eq!(fake.demand, Some(false));
    }

    #[test]
    fn recovered_state_replays_off_and_still_follows_demand() {
        // After recovery the cached state is degraded=false, demand=false; the
        // fresh channel starts idle and later demand drives it normally.
        let mut state = CaptureDemandState::default();
        state.record_degraded(false);
        state.record_demand(false);

        let mut fake = FakeStreamer {
            degraded: None,
            demand: None,
        };
        state.replay_on(&mut fake);
        assert_eq!(fake.degraded, Some(false));
        assert_eq!(fake.demand, Some(false));

        state.record_demand(true);
        state.replay_on(&mut fake);
        assert_eq!(fake.demand, Some(true));
    }
}
