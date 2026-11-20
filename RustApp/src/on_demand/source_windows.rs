//! Windows WASAPI session source.
//!
//! The source runs on the dedicated watcher thread (see `watcher.rs`), so the
//! COM interfaces never cross an apartment boundary. Every fallible COM step is
//! mapped to [`WatcherError`]; the watcher then fails open to always-on capture
//! instead of panicking.
//!
//! Endpoint matching: cpal lists capture devices with their friendly names and
//! WASAPI ids; the friendly name selects the configured/default cable, and the
//! WASAPI id matches the `IMMDevice` enumerated through the COM collection.

use super::{Consumer, OnDemandCaptureConfig, SessionSource, WatcherError, select_endpoint};

use cpal::traits::{DeviceTrait, HostTrait};

use windows::core::Interface;
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, DEVICE_STATE_ACTIVE, IAudioSessionControl,
    IAudioSessionControl2, IAudioSessionEnumerator, IAudioSessionManager2, IMMDevice,
    IMMDeviceCollection, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, CoCreateInstance, CoInitializeEx, COINIT_MULTITHREADED,
};

pub struct WindowsSessionSource {
    /// WASAPI id of the monitored capture endpoint.
    endpoint_id: String,
    com_ready: bool,
    enumerator: Option<IMMDeviceEnumerator>,
    manager: Option<IAudioSessionManager2>,
}

impl WindowsSessionSource {
    pub fn new(config: &OnDemandCaptureConfig) -> Result<Self, WatcherError> {
        let host = cpal::default_host();
        let devices: Vec<(String, String)> = host
            .input_devices()
            .map_err(|_| WatcherError::EnumerationFailed)?
            .filter_map(|device| {
                let description = device.description().ok()?;
                let name = description
                    .extended()
                    .next()
                    .map(String::from)
                    .unwrap_or_else(|| description.name().to_owned());
                let id = device.id().ok()?.to_string();
                Some((name, id))
            })
            .collect();

        let names: Vec<String> = devices.iter().map(|(name, _)| name.clone()).collect();
        let picked_name = select_endpoint(&names, config.endpoint_hint())
            .ok_or(WatcherError::EndpointNotFound)?;
        let endpoint_id = devices
            .into_iter()
            .find(|(name, _)| name == picked_name)
            .map(|(_, id)| id)
            .ok_or(WatcherError::EndpointNotFound)?;

        Ok(Self {
            endpoint_id,
            com_ready: false,
            enumerator: None,
            manager: None,
        })
    }

    fn ensure_com(&mut self) -> Result<(), WatcherError> {
        if !self.com_ready {
            // MTA on this thread; the watcher thread owns all COM objects.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            self.com_ready = true;
        }
        Ok(())
    }

    fn ensure_enumerator(&mut self) -> Result<&IMMDeviceEnumerator, WatcherError> {
        if self.enumerator.is_none() {
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                    .map_err(|_| WatcherError::EnumerationFailed)?;
            self.enumerator = Some(enumerator);
        }
        Ok(self.enumerator.as_ref().expect("enumerator just created"))
    }

    fn ensure_manager(&mut self) -> Result<&IAudioSessionManager2, WatcherError> {
        if self.manager.is_none() {
            let enumerator = self.ensure_enumerator()?;
            let collection: IMMDeviceCollection = unsafe {
                enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            }
            .map_err(|_| WatcherError::EnumerationFailed)?;

            let count = unsafe { collection.GetCount() }
                .map_err(|_| WatcherError::EnumerationFailed)?;
            let mut matched: Option<IMMDevice> = None;

            for index in 0..count {
                let device = unsafe { collection.Item(index) }
                    .map_err(|_| WatcherError::EnumerationFailed)?;
                let device_id = unsafe { device.GetId() }
                    .map_err(|_| WatcherError::EnumerationFailed)?;
                if pwstr_to_string(device_id) == self.endpoint_id {
                    matched = Some(device);
                    break;
                }
            }

            let device = matched.ok_or(WatcherError::EndpointNotFound)?;
            let manager: IAudioSessionManager2 =
                unsafe { device.Activate(CLSCTX_ALL, None) }
                    .map_err(|_| WatcherError::EnumerationFailed)?;
            self.manager = Some(manager);
        }
        Ok(self.manager.as_ref().expect("manager just created"))
    }
}

impl SessionSource for WindowsSessionSource {
    fn snapshot(&mut self) -> Result<Vec<Consumer>, WatcherError> {
        self.ensure_com()?;
        let manager = self.ensure_manager()?;

        // SAFETY: every COM call runs on the watcher thread that owns the
        // cached interfaces; pointers come from successful COM queries.
        unsafe {
            let sessions: IAudioSessionEnumerator = manager
                .GetSessionEnumerator()
                .map_err(|_| WatcherError::EnumerationFailed)?;
            let count = sessions
                .GetCount()
                .map_err(|_| WatcherError::EnumerationFailed)?;

            let mut consumers = Vec::new();
            for index in 0..count {
                let control: IAudioSessionControl = sessions
                    .GetSession(index)
                    .map_err(|_| WatcherError::EnumerationFailed)?;
                let state = control
                    .GetState()
                    .map_err(|_| WatcherError::EnumerationFailed)?;
                if state != AudioSessionStateActive {
                    continue;
                }

                let control2: IAudioSessionControl2 = control
                    .cast()
                    .map_err(|_| WatcherError::EnumerationFailed)?;
                let pid = control2
                    .GetProcessId()
                    .map_err(|_| WatcherError::EnumerationFailed)?;
                consumers.push(Consumer {
                    pid,
                    session: index as u32,
                });
            }

            Ok(consumers)
        }
    }
}

/// Read a NUL-terminated wide string returned by a COM call into a lossy
/// Rust `String`. The COM buffer is owned by the caller and leaked here:
/// these ids are short and enumerated once per cached connection.
fn pwstr_to_string(p: windows::core::PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }

    // SAFETY: the pointer came from a successful `GetId` and is valid for the
    // NUL-terminated wide string; iteration stops at the first zero u16.
    unsafe {
        let mut wide = Vec::new();
        let mut ptr = p.0;
        while !(*ptr).eq(&0) {
            wide.push(*ptr);
            ptr = ptr.add(1);
        }
        String::from_utf16_lossy(&wide)
    }
}
