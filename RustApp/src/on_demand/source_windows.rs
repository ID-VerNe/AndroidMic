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
                // `DeviceId::id()` returns the raw backend id; `to_string()`
                // would render the `wasapi:` host prefix. Keep the raw id so
                // it can be compared directly against the COM `GetId()` string.
                let id = device.id().ok()?.id().to_owned();
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
                if endpoint_ids_match(&pwstr_to_string(device_id), &self.endpoint_id) {
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

/// Strip the cpal host prefix from a WASAPI device id.
///
/// cpal 0.18 `DeviceId::Display` renders `<host>:<id>` (e.g.
/// `wasapi:{0.0.0....}.{guid}`), while the WASAPI backend stores the raw COM
/// `GetId()` string and `IMMDevice::GetId()` returns the bare id. Normalize
/// both sides before comparing so a prefixed and a bare id match.
fn normalize_wasapi_id(id: &str) -> String {
    let trimmed = id.trim();
    if trimmed
        .to_ascii_lowercase()
        .strip_prefix("wasapi:")
        .is_some()
    {
        trimmed["wasapi:".len()..].trim().to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Compare a COM `GetId()` string against a cpal device id, ignoring the
/// optional `wasapi:` host prefix and surrounding whitespace. Friendly names
/// are selected separately by [`select_endpoint`]; they never match an id here.
fn endpoint_ids_match(com_id: &str, cpal_id: &str) -> bool {
    normalize_wasapi_id(com_id) == normalize_wasapi_id(cpal_id)
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

#[cfg(test)]
mod tests {
    use super::*;

    const RAW_ID: &str = "{0.0.0.00000000}.{a498ec47-0ec5-4861-8819-338fba42d4ca}";

    #[test]
    fn prefixed_cpal_id_matches_bare_com_id() {
        // cpal `DeviceId::to_string()` carries the `wasapi:` host prefix; the
        // COM `GetId()` string is bare. The comparison must strip the prefix.
        assert!(endpoint_ids_match(RAW_ID, &format!("wasapi:{RAW_ID}")));
    }

    #[test]
    fn bare_ids_match_without_prefix() {
        assert!(endpoint_ids_match(RAW_ID, RAW_ID));
    }

    #[test]
    fn friendly_name_is_not_treated_as_an_endpoint_id() {
        // Friendly-name selection is the `select_endpoint` substring path; a
        // name must never falsely match a COM endpoint id.
        assert!(!endpoint_ids_match(
            RAW_ID,
            "CABLE Output (VB-Audio Virtual Cable)"
        ));
    }

    #[test]
    fn normalization_trims_and_ignores_prefix_case() {
        assert!(endpoint_ids_match(
            &format!("  {RAW_ID}  "),
            &format!("WASAPI:{RAW_ID}")
        ));
    }
}
