use std::{collections::HashMap, sync::LazyLock};

use cosmic::{
    Element,
    iced::{
        Length,
        alignment::{Horizontal, Vertical},
        widget::{pick_list, text_input},
    },
    widget::{
        self, about::About, button, canvas, column, container, context_menu, horizontal_space,
        markdown, menu, radio, row, scrollable, settings, text, toggler, tooltip,
    },
};
use cpal::traits::DeviceTrait;

use super::{
    app::{AppState, ConnectionState, OnDemandView},
    message::{AppMsg, ConfigMsg},
};
use crate::{
    config::{
        AppTheme, AudioEffect, AudioFormat, ChannelCount, ConnectionMode, DenoiseKind, SampleRate,
    },
    fl,
    ui::message::MenuMsg,
    utils::APP,
    widget_icon_button, widget_icon_handle,
};

pub static SCROLLABLE_ID: LazyLock<cosmic::widget::Id> = LazyLock::new(cosmic::widget::Id::unique);

pub fn main_window(app: &AppState) -> Element<'_, AppMsg> {
    let connection_mode = app.config.data().connection_mode;

    row()
        .padding(50)
        .spacing(50)
        .push(
            column()
                .width(Length::FillPortion(2))
                .height(Length::Fill)
                .spacing(50)
                .push(logs(app))
                .push(wave(app)),
        )
        .push(
            // The settings column is taller than the default 800x600 window once
            // the connection-type hints are in it, and a plain `column` clips its
            // tail: the Connect button and the status line became unreachable.
            // Scroll it so every control stays reachable at any window size.
            scrollable(
                column()
                    .width(Length::Fill)
                    .spacing(20)
                    .align_x(Horizontal::Center)
                    .push_maybe(uses_lan(connection_mode).then(|| network_adapter(app)))
                    .push(audio(app))
                    .push_maybe(app.on_demand_view().map(on_demand_status))
                    .push(connection_type(app)),
            )
            .width(Length::FillPortion(1))
            .height(Length::Fill),
        )
        .into()
}

fn logs(app: &AppState) -> Element<'_, AppMsg> {
    context_menu(
        container(
            scrollable(
                container(
                    markdown::view(
                        &app.logs,
                        markdown::Settings::with_text_size(12),
                        markdown::Style::from_palette(
                            cosmic::iced::Theme::TokyoNightStorm.palette(),
                        ),
                    )
                    .map(|url| AppMsg::LinkClicked(url.to_string())),
                )
                .width(Length::Fill),
            )
            .id(SCROLLABLE_ID.clone()),
        )
        .width(Length::Fill)
        .height(Length::FillPortion(3))
        .padding(13)
        .class(cosmic::theme::Container::Card),
        Some(menu::items(
            &HashMap::new(),
            vec![menu::Item::Button(
                fl!("clear_logs"),
                None,
                MenuMsg::ClearLogs,
            )],
        )),
    )
    .into()
}

fn wave(app: &AppState) -> Element<'_, AppMsg> {
    container(canvas(&app.audio_wave).width(Length::Fill))
        .width(Length::Fill)
        .height(Length::FillPortion(1))
        .into()
}

fn audio(app: &AppState) -> Element<'_, AppMsg> {
    #[cfg(target_os = "linux")]
    let selected_host = app
        .available_hosts
        .iter()
        .find(|d| **d == app.audio_host.id());

    let selected_device = app
        .audio_device
        .as_ref()
        .and_then(|d| d.id().ok())
        .and_then(|id| app.audio_devices.iter().find(|d| d.id == id.to_string()));

    column()
        .spacing(20)
        .align_x(Horizontal::Center)
        .push(text::title4(fl!("audio_device")))
        .push_maybe({
            #[cfg(target_os = "linux")]
            {
                Some(
                    tooltip(
                        pick_list(
                            app.available_hosts.clone(),
                            selected_host,
                            AppMsg::SelectedHost,
                        )
                        .width(Length::Fill),
                        selected_host.as_ref().map_or("None", |host| &host.name()),
                        tooltip::Position::Top,
                    )
                    .class(cosmic::theme::Container::Tooltip),
                )
            }
            #[cfg(not(target_os = "linux"))]
            {
                Option::<Element<AppMsg>>::None
            }
        })
        .push(
            row()
                .width(Length::Fill)
                .spacing(5)
                .push(
                    tooltip(
                        pick_list(app.audio_devices.clone(), selected_device, AppMsg::Device)
                            .width(Length::Fill),
                        selected_device
                            .as_ref()
                            .map_or("None", |device| &device.name),
                        tooltip::Position::Top,
                    )
                    .class(cosmic::theme::Container::Tooltip),
                )
                .push(
                    widget_icon_button!("refresh24")
                        .on_press(AppMsg::RefreshAudioDevices)
                        .class(cosmic::theme::Button::Text)
                        .width(Length::Shrink),
                ),
        )
        .push(button::text(fl!("settings")).on_press(AppMsg::ToggleSettingsWindow))
        .into()
}

/// Whether the mode can reach the PC over the LAN, which is when the adapter
/// choice matters. `Automatic` needs it too: the phone falls back to that
/// address once the cable is out.
fn uses_lan(mode: ConnectionMode) -> bool {
    match mode {
        ConnectionMode::Tcp | ConnectionMode::Udp => true,
        #[cfg(feature = "adb")]
        ConnectionMode::Auto => true,
        _ => false,
    }
}

#[cfg(feature = "adb")]
fn is_auto(mode: ConnectionMode) -> bool {
    matches!(mode, ConnectionMode::Auto)
}

#[cfg(not(feature = "adb"))]
fn is_auto(_mode: ConnectionMode) -> bool {
    false
}

fn network_adapter(app: &AppState) -> Element<'_, AppMsg> {
    let selected = app.network_adapter.as_ref();
    let auto = is_auto(app.config.data().connection_mode);
    column()
        .spacing(20)
        .align_x(Horizontal::Center)
        .push(text::title4(if auto {
            fl!("fallback_address")
        } else {
            fl!("network_adapter")
        }))
        .push(
            row()
                .width(Length::Fill)
                .spacing(5)
                .push(
                    tooltip(
                        pick_list(app.network_adapters.clone(), selected, AppMsg::Adapter)
                            .width(Length::Fill),
                        selected.map_or("None", |adapter| &adapter.name),
                        tooltip::Position::Top,
                    )
                    .class(cosmic::theme::Container::Tooltip),
                )
                .push(
                    widget_icon_button!("refresh24")
                        .on_press(AppMsg::RefreshNetworkAdapters)
                        .class(cosmic::theme::Button::Text)
                        .width(Length::Shrink),
                ),
        )
        .push_maybe(auto.then(|| text::caption(fl!("fallback_address_hint"))))
        .into()
}

fn connection_type(app: &AppState) -> Element<'_, AppMsg> {
    let connection_mode = app.config.data().connection_mode;
    let auto = is_auto(connection_mode);
    #[cfg(feature = "adb")]
    let manual_mode = app.manual_mode;

    // Grouped by policy, not by transport: the top-level choice is an outcome
    // ("Automatic" / "Specify manually"), the protocols only matter once the
    // user opts out of it.
    let mut modes = column().spacing(5);

    #[cfg(feature = "adb")]
    {
        modes = modes
            .push(
                radio(
                    text(fl!("connection_auto")),
                    true,
                    Some(auto),
                    |_| AppMsg::ChangeConnectionMode(ConnectionMode::Auto),
                )
                .width(Length::Fill),
            )
            .push_maybe(auto.then(|| text::caption(fl!("connection_auto_hint"))))
            .push(
                radio(
                    text(fl!("connection_manual")),
                    false,
                    Some(auto),
                    move |_| AppMsg::ChangeConnectionMode(manual_mode),
                )
                .width(Length::Fill),
            );
    }

    if !auto {
        let mut manual = column().spacing(5).padding([0, 20]);
        manual = manual.push(radio(
            text(fl!("connection_tcp")),
            ConnectionMode::Tcp,
            Some(connection_mode),
            AppMsg::ChangeConnectionMode,
        ));
        manual = manual.push(radio(
            text(fl!("connection_udp")),
            ConnectionMode::Udp,
            Some(connection_mode),
            AppMsg::ChangeConnectionMode,
        ));
        #[cfg(feature = "usb")]
        {
            manual = manual.push(radio(
                text(fl!("connection_usb")),
                ConnectionMode::Usb,
                Some(connection_mode),
                AppMsg::ChangeConnectionMode,
            ));
        }
        #[cfg(feature = "adb")]
        {
            manual = manual.push(radio(
                text(fl!("connection_adb")),
                ConnectionMode::Adb,
                Some(connection_mode),
                AppMsg::ChangeConnectionMode,
            ));
        }
        modes = modes.push(manual);
    }

    column()
        .spacing(20)
        .align_x(Horizontal::Center)
        .push(text::title4(fl!("connection")))
        .push(modes)
        .push(connect_button(app))
        .push(text(app.status_text()))
        .push_maybe(
            (auto && app.connection_state == ConnectionState::Listening)
                .then(|| text::caption(fl!("listening_auto_hint"))),
        )
        .into()
}

fn connect_button(app: &AppState) -> Element<'_, AppMsg> {
    match app.connection_state {
        ConnectionState::Default => button::text(fl!("connect")).on_press(AppMsg::Connect),
        ConnectionState::Listening => button::text(fl!("listening")).on_press(AppMsg::Stop),
        ConnectionState::Connected => button::destructive(fl!("disconnect")).on_press(AppMsg::Stop),
        ConnectionState::WaitingOnStatus => button::text(fl!("waiting")),
    }
    .into()
}

fn on_demand_status(view: OnDemandView) -> Element<'static, AppMsg> {
    let label = match view {
        OnDemandView::Standby => fl!("on_demand_state_standby"),
        OnDemandView::Capturing => fl!("on_demand_state_capturing"),
        OnDemandView::Degraded => fl!("on_demand_state_degraded"),
        OnDemandView::Legacy => fl!("on_demand_state_legacy"),
    };

    column()
        .spacing(10)
        .align_x(Horizontal::Center)
        .push(text::title4(fl!("on_demand")))
        .push(text(label))
        .into()
}

pub fn settings_window(app: &AppState) -> Element<'_, ConfigMsg> {
    let config = app.config.data();

    scrollable(
        column()
            .padding(50)
            .spacing(20)
            .push(
                settings::section()
                    .title(fl!("title_audio_format"))
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("sample_rate")))
                            .push(horizontal_space())
                            .push(pick_list(
                                SampleRate::VALUES,
                                Some(&config.sample_rate),
                                ConfigMsg::SampleRate,
                            )),
                    )
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("channel_count")))
                            .push(horizontal_space())
                            .push(pick_list(
                                ChannelCount::VALUES,
                                Some(&config.channel_count),
                                ConfigMsg::ChannelCount,
                            )),
                    )
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("audio_format")))
                            .push(horizontal_space())
                            .push(pick_list(
                                AudioFormat::VALUES,
                                Some(&config.audio_format),
                                ConfigMsg::AudioFormat,
                            )),
                    )
                    .add(
                        row()
                            .push(horizontal_space())
                            .push(
                                button::text(fl!("use_recommended_audio_format"))
                                    .on_press(ConfigMsg::UseRecommendedFormat),
                            )
                            .push(horizontal_space()),
                    ),
            )
            .push(
                settings::section()
                    .title(fl!("title_connection"))
                    .add(
                        row()
                            .width(Length::Fill)
                            .align_y(Vertical::Center)
                            .spacing(5)
                            .push(text(fl!("port")))
                            .push(horizontal_space())
                            .push(
                                text_input("", &app.port_input)
                                    .on_input(ConfigMsg::PortTextInput)
                                    .width(Length::Fixed(150.0)),
                            )
                            .push(button::text(fl!("save")).on_press(ConfigMsg::PortSave)),
                    )
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .spacing(10)
                            .push(text(fl!("target_latency")))
                            .push(text(format!("{} ms", config.target_latency_ms)))
                            .push(
                                widget::slider(
                                    5..=500,
                                    config.target_latency_ms,
                                    ConfigMsg::TargetLatency,
                                )
                                .step(5u32)
                                .on_release(ConfigMsg::TargetLatencyApply),
                            ),
                    ),
            )
            .push(
                settings::section()
                    .title(fl!("on_demand"))
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("on_demand_enabled")))
                            .push(horizontal_space())
                            .push(
                                toggler(config.on_demand_capture.enabled)
                                    .on_toggle(ConfigMsg::OnDemandEnabled),
                            ),
                    )
                    .add_maybe(config.on_demand_capture.enabled.then(|| {
                        row()
                            .width(Length::Fill)
                            .align_y(Vertical::Center)
                            .spacing(5)
                            .push(text(fl!("on_demand_endpoint")))
                            .push(horizontal_space())
                            .push(
                                text_input("", &app.on_demand_endpoint_input)
                                    .on_input(ConfigMsg::OnDemandEndpointInput)
                                    .width(Length::Fixed(150.0)),
                            )
                            .push(
                                button::text(fl!("save"))
                                    .on_press(ConfigMsg::OnDemandEndpointSave),
                            )
                    })),
            )
            .push(
                settings::section()
                    .title(fl!("denoise"))
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("denoise_enabled")))
                            .push(horizontal_space())
                            .push(toggler(config.denoise).on_toggle(ConfigMsg::DeNoise)),
                    )
                    .add_maybe((config.denoise).then(|| {
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("denoise_type")))
                            .push(horizontal_space())
                            .push(pick_list(
                                DenoiseKind::VALUES,
                                Some(&config.denoise_kind),
                                ConfigMsg::DeNoiseKind,
                            ))
                    }))
                    .add_maybe(
                        (config.denoise && config.denoise_kind == DenoiseKind::Speexdsp).then(
                            || {
                                row()
                                    .align_y(Vertical::Center)
                                    .spacing(10)
                                    .push(text(format!("{} db", config.speex_noise_suppress)))
                                    .push(
                                        widget::slider(
                                            -100..=0,
                                            config.speex_noise_suppress,
                                            ConfigMsg::SpeexNoiseSuppress,
                                        )
                                        .step(1),
                                    )
                            },
                        ),
                    ),
            )
            .push(
                settings::section()
                    .title(fl!("voice_activity_detection"))
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("voice_activity_detection_enabled")))
                            .push(horizontal_space())
                            .push(
                                toggler(config.speex_vad_enabled)
                                    .on_toggle(ConfigMsg::SpeexVADEnabled),
                            ),
                    )
                    .add_maybe((config.speex_vad_enabled).then(|| {
                        row()
                            .align_y(Vertical::Center)
                            .spacing(10)
                            .push(text(format!("{}", config.speex_vad_threshold)))
                            .push(
                                widget::slider(
                                    0..=100,
                                    config.speex_vad_threshold as i32,
                                    ConfigMsg::SpeexVADThreshold,
                                )
                                .step(1),
                            )
                    })),
            )
            .push(
                settings::section()
                    .title(fl!("gain_control"))
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("auto_gain_control")))
                            .push(horizontal_space())
                            .push(
                                toggler(config.speex_agc_enabled)
                                    .on_toggle(ConfigMsg::SpeexAGCEnabled),
                            ),
                    )
                    .add_maybe((config.speex_agc_enabled).then(|| {
                        row()
                            .align_y(Vertical::Center)
                            .spacing(10)
                            .push(text(format!("{}", config.speex_agc_target)))
                            .push(
                                widget::slider(
                                    8000..=65535,
                                    config.speex_agc_target as i32,
                                    ConfigMsg::SpeexAGCTarget,
                                )
                                .step(1),
                            )
                    }))
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("amplify")))
                            .push(horizontal_space())
                            .push(toggler(config.amplify).on_toggle(ConfigMsg::Amplify)),
                    )
                    .add_maybe((config.amplify).then(|| {
                        row()
                            .align_y(Vertical::Center)
                            .spacing(10)
                            .push(text(format!("{:.1}", config.amplify_value)))
                            .push(
                                widget::slider(
                                    0.0..=10.0,
                                    config.amplify_value,
                                    ConfigMsg::AmplifyValue,
                                )
                                .step(0.1_f32),
                            )
                    })),
            )
            .push(
                settings::section()
                    .title(fl!("dereverberation"))
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("dereverberation_enabled")))
                            .push(horizontal_space())
                            .push(
                                toggler(config.speex_dereverb_enabled)
                                    .on_toggle(ConfigMsg::SpeexDereverbEnabled),
                            ),
                    )
                    .add_maybe((config.speex_dereverb_enabled).then(|| {
                        row()
                            .align_y(Vertical::Center)
                            .spacing(10)
                            .push(text(format!("{:.1}", config.speex_dereverb_level)))
                            .push(
                                widget::slider(
                                    0.0..=1.0,
                                    config.speex_dereverb_level,
                                    ConfigMsg::SpeexDereverbLevel,
                                )
                                .step(0.1_f32),
                            )
                    })),
            )
            .push(
                button::text(fl!("reset_denoise_settings"))
                    .on_press(ConfigMsg::ResetDenoiseSettings),
            )
            .push(
                settings::section().title("Audio Effect").add(
                    row()
                        .align_y(Vertical::Center)
                        .push(text("Audio Effect"))
                        .push(horizontal_space())
                        .push(pick_list(
                            AudioEffect::VALUES,
                            Some(&config.post_effect),
                            ConfigMsg::PostAudioEffect,
                        )),
                ),
            )
            .push(
                settings::section()
                    .title(fl!("title_app"))
                    .add_maybe(if cfg!(target_os = "windows") {
                        Some(
                            row()
                                .align_y(Vertical::Center)
                                .push(text(fl!("start_at_login")))
                                .push(horizontal_space())
                                .push(
                                    toggler(config.start_at_login)
                                        .on_toggle(ConfigMsg::StartAtLogin),
                                ),
                        )
                    } else {
                        None
                    })
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("start_minimized")))
                            .push(horizontal_space())
                            .push(
                                toggler(config.start_minimized)
                                    .on_toggle(ConfigMsg::StartMinimized),
                            ),
                    )
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("auto_connect")))
                            .push(horizontal_space())
                            .push(toggler(config.auto_connect).on_toggle(ConfigMsg::AutoConnect)),
                    )
                    .add(
                        row()
                            .align_y(Vertical::Center)
                            .push(text(fl!("theme")))
                            .push(horizontal_space())
                            .push(pick_list(
                                AppTheme::VALUES,
                                Some(&config.theme),
                                ConfigMsg::Theme,
                            )),
                    )
                    .add(widget::settings::item::builder(fl!("about")).control(
                        button::text(fl!("about_open")).on_press(ConfigMsg::ToggleAboutWindow),
                    )),
            ),
    )
    .into()
}

static ABOUT: LazyLock<About> = LazyLock::new(|| {
    About::default()
        .name(APP)
        .icon(widget_icon_handle!("icon"))
        .license("GPL-3.0-only")
        .links([
            (
                fl!("repository"),
                "https://github.com/teamclouday/AndroidMic",
            ),
            (
                fl!("issues_tracker"),
                "https://github.com/teamclouday/AndroidMic/issues",
            ),
        ])
        .developers([
            ("wiiznokes", "wiiznokes2@gmail.com"),
            ("teamclouday", "teamclouday@gmail.com"),
        ])
        .version(format!(
            "{}-{}",
            env!("CARGO_PKG_VERSION"),
            env!("ANDROID_MIC_COMMIT")
        ))
});

pub fn about_window() -> Element<'static, AppMsg> {
    container(widget::about(&ABOUT, |url| {
        AppMsg::LinkClicked(url.to_owned())
    }))
    .padding(20)
    .into()
}
