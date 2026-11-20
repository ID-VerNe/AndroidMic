//! Pure demand state machine for on-demand capture.
//!
//! The machine only sees snapshots of actively recording consumers on the
//! monitored endpoint; all Windows/WASAPI interaction lives in the source
//! layer, so this can be exercised headlessly with a fake clock.
//!
//! Fail-open contract: the machine itself never returns an error. Callers map
//! source failures to the degraded (always-on) state before feeding it here.

use std::collections::{HashMap, HashSet};

/// A recording session on the monitored capture endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Consumer {
    /// Owning process id; `0` marks the system/unknown session and is ignored.
    pub pid: u32,
    /// WASAPI session id, kept so several streams from the same process can be
    /// told apart.
    pub session: u32,
}

/// Why the watcher fell back to always-on. Reported to the UI/log, never
/// fatal: audio keeps flowing in every case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatcherError {
    /// Current platform has no session watcher implementation (non-Windows).
    UnsupportedPlatform,
    /// Configured / guessed capture endpoint was not found.
    EndpointNotFound,
    /// COM enumeration failed for another reason.
    EnumerationFailed,
}

/// Off-debounce: how long demand stays on after the last consumer leaves.
pub const OFF_DEBOUNCE_MS: u64 = 300;
/// Off-debounce once a pid has triggered enough rising edges in the window.
pub const EXTENDED_OFF_DEBOUNCE_MS: u64 = 2000;
/// Rising edges per pid in the window that switch to the extended debounce.
pub const EXTENDED_EDGE_COUNT: usize = 4;
/// Sliding window over which rising edges are counted.
pub const EDGE_WINDOW_MS: u64 = 5000;
/// Continuous no-demand period after which the edge counters are reset.
pub const IDLE_RESET_MS: u64 = 5000;

/// Drop the system/unknown session (`pid == 0`) and sessions owned by this
/// process itself; otherwise a stale self/session would pin demand on forever.
pub fn filter_consumers(consumers: &[Consumer], self_pid: u32) -> Vec<Consumer> {
    consumers
        .iter()
        .copied()
        .filter(|consumer| consumer.pid != 0 && consumer.pid != self_pid)
        .collect()
}

/// Debounced demand state machine, driven by periodic snapshots.
#[derive(Debug)]
pub struct DemandMachine {
    self_pid: u32,
    /// Pids active in the previous raw snapshot (before debounce).
    previous_active: HashSet<u32>,
    /// Debounced output: true when the phone may capture.
    current: bool,
    /// Timestamp until which demand stays on after consumers disappeared.
    off_deadline: Option<u64>,
    /// Per-pid rising edge timestamps inside the sliding window.
    edges: HashMap<u32, Vec<u64>>,
    /// When demand first went false; drives the idle edge-counter reset.
    demand_false_since: Option<u64>,
}

impl DemandMachine {
    pub fn new(self_pid: u32) -> Self {
        Self {
            self_pid,
            previous_active: HashSet::new(),
            current: false,
            off_deadline: None,
            edges: HashMap::new(),
            demand_false_since: None,
        }
    }

    /// Debounced demand after applying the latest snapshot.
    pub fn demand(&self) -> bool {
        self.current
    }

    /// Feed a fresh snapshot of active consumers. `now` is a monotonic clock
    /// in milliseconds. Returns the debounced demand.
    pub fn update(&mut self, consumers: &[Consumer], now: u64) -> bool {
        let filtered = filter_consumers(consumers, self.self_pid);
        let active: HashSet<u32> = filtered.iter().map(|consumer| consumer.pid).collect();

        // Record rising edges: a pid that was inactive and is active now.
        for pid in &active {
            if !self.previous_active.contains(pid) {
                let edges = self.edges.entry(*pid).or_default();
                edges.push(now);
                edges.retain(|&timestamp| now - timestamp <= EDGE_WINDOW_MS);
            }
        }

        let target = !active.is_empty();

        if target {
            self.demand_false_since = None;
            self.off_deadline = None;
            self.current = true;
        } else {
            if self.demand_false_since.is_none() {
                self.demand_false_since = Some(now);
            }
            // After a long idle period the edge history no longer reflects
            // current behaviour; start counting fresh.
            if now - self.demand_false_since.unwrap_or(now) >= IDLE_RESET_MS {
                self.edges.clear();
            }

            if self.current && self.off_deadline.is_none() {
                let extended = self
                    .edges
                    .values()
                    .any(|edges| edges.len() >= EXTENDED_EDGE_COUNT);
                let delay = if extended {
                    EXTENDED_OFF_DEBOUNCE_MS
                } else {
                    OFF_DEBOUNCE_MS
                };
                self.off_deadline = Some(now + delay);
            }

            match self.off_deadline {
                Some(deadline) if now < deadline => self.current = true,
                _ => {
                    self.current = false;
                    self.off_deadline = None;
                }
            }
        }

        self.previous_active = active;
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SELF_PID: u32 = 1234;

    fn consumer(pid: u32) -> Consumer {
        Consumer { pid, session: pid }
    }

    #[test]
    fn filters_pid_zero_and_self_process() {
        let snapshot = [
            consumer(0),            // system/unknown session
            consumer(SELF_PID),     // our own process
            consumer(42),           // real client
            Consumer { pid: 0, session: 7 },
        ];

        let filtered = filter_consumers(&snapshot, SELF_PID);
        assert_eq!(filtered, vec![consumer(42)]);
    }

    #[test]
    fn demand_follows_active_consumer_immediately() {
        let mut machine = DemandMachine::new(SELF_PID);

        assert!(!machine.update(&[], 0));
        assert!(machine.update(&[consumer(42)], 100));
        assert!(machine.demand());
    }

    #[test]
    fn off_debounce_keeps_demand_for_300ms() {
        let mut machine = DemandMachine::new(SELF_PID);
        machine.update(&[consumer(42)], 0);

        // consumer leaves at t=100
        assert!(machine.update(&[], 100));
        // still inside the 300ms off-debounce window
        assert!(machine.update(&[], 399));
        // past the deadline
        assert!(!machine.update(&[], 400));
    }

    #[test]
    fn per_session_and_per_pid_tracking() {
        let mut machine = DemandMachine::new(SELF_PID);

        // Two sessions from the same pid: leaving one keeps demand on.
        machine.update(&[consumer(42), Consumer { pid: 42, session: 9 }], 0);
        assert!(machine.update(&[consumer(42)], 100));

        // A different pid arriving is a fresh rising edge for that pid.
        assert!(machine.update(&[consumer(43)], 200));
    }

    #[test]
    fn fourth_rising_edge_in_window_extends_debounce_to_2s() {
        let mut machine = DemandMachine::new(SELF_PID);

        // Four rapid on/off cycles for pid 42 inside the 5s window.
        let mut now = 0u64;
        for _ in 0..4 {
            machine.update(&[consumer(42)], now);
            now += 100;
            machine.update(&[], now);
            now += 100;
        }

        // Extended debounce: still on at 1.5s after the last leave.
        assert!(machine.update(&[], now + 1_500));
        // But past 2s it drops.
        assert!(!machine.update(&[], now + 2_001));
    }

    #[test]
    fn idle_period_resets_edge_history() {
        let mut machine = DemandMachine::new(SELF_PID);

        for _ in 0..4 {
            machine.update(&[consumer(42)], 0);
            machine.update(&[], 100);
        }

        // Long idle: edge counters reset, next off uses the short debounce.
        machine.update(&[], IDLE_RESET_MS + 100);
        machine.update(&[consumer(42)], IDLE_RESET_MS + 200);
        machine.update(&[], IDLE_RESET_MS + 300);

        // Short 300ms debounce applies again, not the 2s extended one.
        assert!(machine.update(&[], IDLE_RESET_MS + 599));
        assert!(!machine.update(&[], IDLE_RESET_MS + 600));
    }

    #[test]
    fn session_level_demand_waits_for_all_sessions_of_a_pid() {
        let mut machine = DemandMachine::new(SELF_PID);

        machine.update(
            &[Consumer { pid: 42, session: 1 }, Consumer { pid: 42, session: 2 }],
            0,
        );
        // One session leaves while the other is still recording: demand stays.
        assert!(machine.update(&[Consumer { pid: 42, session: 2 }], 100));
        // Both leave: the short off-debounce keeps demand briefly, then it drops.
        assert!(machine.update(&[], 100));
        assert!(!machine.update(&[], 500));
    }

    #[test]
    fn empty_snapshot_after_start_stays_off() {
        let mut machine = DemandMachine::new(SELF_PID);
        assert!(!machine.update(&[], 0));
        assert!(!machine.update(&[], 1_000));
    }
}
