//! Capture endpoint selection.
//!
//! The watcher follows a local virtual-cable *capture* endpoint (the paired
//! side of the playback cable AndroidMic writes to). Names are matched as a
//! case-insensitive substring so a configured substring can disambiguate
//! several virtual cables; an empty hint falls back to the common VB-Cable
//! default.

/// Friendly-name substring guessed when the user left `endpoint_id` empty.
pub const DEFAULT_ENDPOINT_HINT: &str = "CABLE Output";

/// Pick the first available endpoint whose name matches the hint.
///
/// An empty `hint` selects the default virtual-cable guess. Returns `None`
/// when nothing matches; the caller fails open to always-on in that case.
pub fn select_endpoint<'a>(available: &'a [String], hint: &str) -> Option<&'a String> {
    let needle = hint.trim();
    let needle = if needle.is_empty() { DEFAULT_ENDPOINT_HINT } else { needle };
    let needle = needle.to_lowercase();

    available
        .iter()
        .find(|name| name.to_lowercase().contains(&needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec![
            "CABLE Input (VB-Audio Virtual Cable)".to_string(),
            "CABLE Output (VB-Audio Virtual Cable)".to_string(),
            "Microphone Array".to_string(),
        ]
    }

    #[test]
    fn empty_hint_picks_default_output_cable() {
        let names = names();
        let picked = select_endpoint(&names, "").unwrap();
        assert_eq!(picked, "CABLE Output (VB-Audio Virtual Cable)");
    }

    #[test]
    fn configured_substring_matches_case_insensitively() {
        let names = names();
        let picked = select_endpoint(&names, "cable output").unwrap();
        assert!(picked.contains("CABLE Output"));
    }

    #[test]
    fn missing_endpoint_returns_none() {
        assert!(select_endpoint(&names(), "Whisper Cable").is_none());
        assert!(select_endpoint(&[], "CABLE Output").is_none());
    }
}
