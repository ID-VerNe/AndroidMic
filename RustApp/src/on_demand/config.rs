use serde::{Deserialize, Serialize};

/// Persisted settings for the on-demand capture feature.
///
/// Stored under the `[on_demand_capture]` TOML section. Old config files that
/// predate this section deserialize with `#[serde(default)]`, so a missing
/// section simply falls back to the defaults below.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OnDemandCaptureConfig {
    /// Master switch. When false the PC never negotiates on-demand capture and
    /// keeps the legacy always-on behaviour.
    pub enabled: bool,
    /// Friendly-name substring of the capture endpoint to watch (e.g.
    /// `CABLE Output`). Empty means "pick the default virtual-cable guess".
    pub endpoint_id: String,
}

impl Default for OnDemandCaptureConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            endpoint_id: String::new(),
        }
    }
}

impl OnDemandCaptureConfig {
    /// Trimmed endpoint hint; empty when the caller should auto-detect.
    pub fn endpoint_hint(&self) -> &str {
        self.endpoint_id.trim()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_enabled_with_empty_endpoint() {
        let config = OnDemandCaptureConfig::default();
        assert!(config.enabled);
        assert!(config.endpoint_id.is_empty());
        assert_eq!(config.endpoint_hint(), "");
    }

    #[test]
    fn endpoint_hint_is_trimmed() {
        let config = OnDemandCaptureConfig {
            enabled: false,
            endpoint_id: "  CABLE Output  ".to_owned(),
        };
        assert_eq!(config.endpoint_hint(), "CABLE Output");
    }
}
