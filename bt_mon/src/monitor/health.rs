//! What a monitor can say about its own ability to hear the radio.
//!
//! [`DeviceEvent`](super::DeviceEvent) is about a device. A radio that was toggled off, a
//! scan that stopped being re-armed, a platform event stream that ended and was never
//! reopened — none of those are reports about a device, so none of them had any way to
//! reach a consumer at all: an adapter that had gone off looked exactly like an empty
//! room, and a node would report zero sightings forever with nothing to report. This is
//! the other channel, and it is deliberately not an event variant: there is no device to
//! put in it, and a consumer that has to poll `device_events()` for a health answer would
//! still be unable to tell "quiet" from "dead".
//!
//! The counters are cheap atomics a long-running consumer can read on its own schedule.
//! A backend that does not track any of this returns `None` from
//! [`DeviceMonitor::health`](super::DeviceMonitor::health) rather than a snapshot full of
//! zeroes, because "everything is false" and "nobody is measuring" have to stay
//! distinguishable — the first is an alarm, the second is a fact about the build.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// A readable copy of a backend's health state.
///
/// Obtain it from [`DeviceMonitor::health`](super::DeviceMonitor::health); the backend
/// keeps the live counters behind it and takes this copy whenever a consumer asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorHealth {
    /// Whether the adapter last reported itself powered on.
    ///
    /// `false` here with `scanning_requested: true` is the case TD-13 was about: the
    /// operator asked for a scan and the radio cannot deliver one.
    pub adapter_powered: bool,
    /// Whether someone has asked this monitor to scan.
    ///
    /// Kept separate from [`Self::discovering`] because the two answer different
    /// questions: one is what was requested, the other is whether it is happening.
    pub scanning_requested: bool,
    /// Whether the radio is believed to be discovering right now: a scan was requested
    /// *and* nothing has since said the radio is unable to deliver reports.
    ///
    /// This is what [`is_scanning`](super::DeviceMonitor::is_scanning) answers with. It
    /// goes false within one scan interval of the adapter going off or of every re-arm
    /// failing, and returns to true on the first re-arm that succeeds — recovery is as
    /// observable as the failure.
    pub discovering: bool,
    /// How many times a running scan has been re-armed, including the attempts that
    /// failed.
    pub rearm_attempts: u64,
    /// How many of those re-arms failed.
    pub rearm_failures: u64,
    /// Re-arm failures in a row, reset by any success.
    ///
    /// One is a D-Bus hiccup; a count that only grows is a radio that is not coming
    /// back, and that is the difference an operator acts on.
    pub consecutive_rearm_failures: u64,
    /// How many times the platform event stream has been opened, counting the first.
    ///
    /// A reopen means the stream ended and the monitor went back for another — normal
    /// after a `bluetoothd` restart. A count climbing every few seconds is a stack that
    /// keeps dropping the subscription, which looks like a working node otherwise.
    pub event_stream_opens: u64,
    /// The most recent failure worth telling an operator about, in the backend's words.
    ///
    /// Sticky: it survives recovery so that a consumer polling every minute still sees
    /// that something went wrong, and gets cleared only by a later failure.
    pub last_error: Option<String>,
}

impl Default for MonitorHealth {
    fn default() -> Self {
        Self {
            adapter_powered: true,
            scanning_requested: false,
            discovering: false,
            rearm_attempts: 0,
            rearm_failures: 0,
            consecutive_rearm_failures: 0,
            event_stream_opens: 0,
            last_error: None,
        }
    }
}

/// The live counters behind a [`MonitorHealth`] snapshot.
///
/// `radio_ready` is the one input no counter can supply: the backend sets it false when
/// the platform says the radio cannot deliver reports (powered off, every re-arm failing)
/// and true again when a report or a successful re-arm says it can.
#[derive(Debug)]
pub(crate) struct HealthState {
    adapter_powered: AtomicBool,
    radio_ready: AtomicBool,
    rearm_attempts: AtomicU64,
    rearm_failures: AtomicU64,
    consecutive_rearm_failures: AtomicU64,
    event_stream_opens: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl Default for HealthState {
    fn default() -> Self {
        Self {
            // A monitor that got far enough to exist has an adapter, and the platform
            // will say so if it is off; assuming "unpowered" here would make every
            // healthy monitor look broken until the first event arrived.
            adapter_powered: AtomicBool::new(true),
            radio_ready: AtomicBool::new(true),
            rearm_attempts: AtomicU64::new(0),
            rearm_failures: AtomicU64::new(0),
            consecutive_rearm_failures: AtomicU64::new(0),
            event_stream_opens: AtomicU64::new(0),
            last_error: Mutex::new(None),
        }
    }
}

impl HealthState {
    /// A state that assumes a powered adapter, which is what a monitor that finished
    /// constructing has just established.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A state for a monitor that knows its adapter is not powered — the mock's
    /// `adapter_powered(false)` switch.
    pub(crate) fn unpowered() -> Self {
        let state = Self::default();
        state.adapter_powered.store(false, Ordering::SeqCst);
        state.radio_ready.store(false, Ordering::SeqCst);
        state
    }

    /// Records what the platform said about power, and what that means for delivery:
    /// an adapter that is off is not delivering reports whatever was asked for.
    pub(crate) fn set_adapter_powered(&self, powered: bool) {
        self.adapter_powered.store(powered, Ordering::SeqCst);
        if !powered {
            self.radio_ready.store(false, Ordering::SeqCst);
        }
    }

    /// Marks the radio able (or unable) to deliver reports, independently of power —
    /// a scan that will not re-arm has an adapter and still hears nothing.
    pub(crate) fn set_radio_ready(&self, ready: bool) {
        self.radio_ready.store(ready, Ordering::SeqCst);
    }

    /// Whether the radio is currently believed able to deliver reports; the backend
    /// ANDs this with its own "someone asked me to scan" flag.
    pub(crate) fn radio_ready(&self) -> bool {
        self.radio_ready.load(Ordering::SeqCst)
    }

    /// A re-arm of a running scan was attempted, whatever it achieves.
    pub(crate) fn note_rearm_attempt(&self) {
        self.rearm_attempts.fetch_add(1, Ordering::SeqCst);
    }

    /// The re-arm failed, so for the moment the radio is not delivering reports.
    ///
    /// One failure is enough to stop claiming a scan is happening — a consumer must be
    /// able to tell within one scan interval (TD-13) — and the streak is what tells a
    /// transient D-Bus hiccup apart from a radio that is not coming back.
    pub(crate) fn note_rearm_failure(&self, error: &str) {
        self.rearm_failures.fetch_add(1, Ordering::SeqCst);
        let consecutive = self
            .consecutive_rearm_failures
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        self.radio_ready.store(false, Ordering::SeqCst);
        self.note_error(format!(
            "scan re-arm failed {consecutive} time(s) in a row: {error}"
        ));
    }

    /// The re-arm worked, so whatever streak of failures there was is over. Recovery is
    /// counted by the attempt total moving, which is why this does not clear it.
    pub(crate) fn note_rearm_success(&self) {
        self.consecutive_rearm_failures.store(0, Ordering::SeqCst);
        self.radio_ready.store(true, Ordering::SeqCst);
    }

    /// A platform event stream was opened. More than one means one ended and the
    /// monitor went back for another.
    pub(crate) fn note_event_stream_open(&self) {
        self.event_stream_opens.fetch_add(1, Ordering::SeqCst);
    }

    /// Remembers the backend's own words about the last thing that went wrong.
    pub(crate) fn note_error(&self, error: impl Into<String>) {
        let message = error.into();
        // A poisoned lock means somebody panicked while naming an error; the health
        // report is not the place to propagate that.
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = Some(message);
        }
    }

    /// The readable copy, given whether a scan has been requested.
    pub(crate) fn snapshot(&self, scanning_requested: bool) -> MonitorHealth {
        MonitorHealth {
            adapter_powered: self.adapter_powered.load(Ordering::SeqCst),
            scanning_requested,
            discovering: scanning_requested && self.radio_ready.load(Ordering::SeqCst),
            rearm_attempts: self.rearm_attempts.load(Ordering::SeqCst),
            rearm_failures: self.rearm_failures.load(Ordering::SeqCst),
            consecutive_rearm_failures: self.consecutive_rearm_failures.load(Ordering::SeqCst),
            event_stream_opens: self.event_stream_opens.load(Ordering::SeqCst),
            last_error: self.last_error.lock().ok().and_then(|slot| slot.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scan_that_cannot_be_rearmed_stops_looking_like_a_scan() {
        // The TD-13 case: the operator asked for a scan, the radio went away, and
        // every observable said the scan was still running.
        let state = HealthState::new();
        assert!(state.snapshot(true).discovering, "a fresh scan is a scan");

        state.note_rearm_attempt();
        state.note_rearm_failure("command not supported");
        state.note_rearm_attempt();
        state.note_rearm_failure("command not supported");

        let health = state.snapshot(true);
        assert_eq!(health.rearm_attempts, 2);
        assert_eq!(health.rearm_failures, 2);
        assert!(
            !health.discovering,
            "a scan that will not restart is not a scan that is happening, whatever \
             the scanning flag still says"
        );
        assert_eq!(
            health.consecutive_rearm_failures, 2,
            "a streak is what distinguishes a hiccup from a dead radio"
        );
        assert!(
            health.last_error.as_deref().unwrap().contains("2 time(s)"),
            "and the message says how many: {:?}",
            health.last_error
        );

        state.note_rearm_attempt();
        state.note_rearm_success();
        let recovered = state.snapshot(true);
        assert_eq!(
            recovered.consecutive_rearm_failures, 0,
            "recovery is observable"
        );
        assert_eq!(recovered.rearm_attempts, 3);
        assert!(
            recovered.last_error.is_some(),
            "the failure stays visible after the recovery, because a consumer that \
             polls every minute would otherwise never learn it happened"
        );
    }

    #[test]
    fn a_radio_that_is_off_is_not_discovering_however_it_got_that_way() {
        let state = HealthState::new();
        state.set_adapter_powered(false);

        let health = state.snapshot(true);
        assert!(!health.adapter_powered);
        assert!(
            health.scanning_requested,
            "the request is remembered separately from whether it can be honoured"
        );
        assert!(
            !health.discovering,
            "a scan asked for on an adapter that is off is not a scan happening"
        );

        state.set_adapter_powered(true);
        let back_on = state.snapshot(true);
        assert!(
            back_on.adapter_powered,
            "the platform's own words about power are recorded"
        );
        assert!(
            !back_on.discovering,
            "power returning does not restart a scan — nothing survived the power cycle — so \
             the monitor does not claim to be discovering until a re-arm says it is"
        );

        state.note_rearm_attempt();
        state.note_rearm_success();
        assert!(
            state.snapshot(true).discovering,
            "and it is the re-arm that brings the scan back, within one interval"
        );
    }

    #[test]
    fn nothing_requested_is_not_discovering_even_with_a_healthy_radio() {
        let state = HealthState::new();
        assert!(!state.snapshot(false).discovering);
        assert!(state.snapshot(false).adapter_powered);
    }

    #[test]
    fn a_monitor_that_never_opened_its_event_stream_says_so() {
        let state = HealthState::new();
        let health = state.snapshot(false);
        assert_eq!(
            health.event_stream_opens, 0,
            "no stream has been opened, which is what a consumer needs to know \
             before it concludes the radio is quiet"
        );

        state.note_event_stream_open();
        state.note_event_stream_open();
        assert_eq!(
            state.snapshot(false).event_stream_opens,
            2,
            "a reopen is a stream that ended and was replaced"
        );
    }
}
