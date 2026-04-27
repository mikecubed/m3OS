//! Single-client policy — Phase 57 Track D.5.
//!
//! At-most-one-client per `audio_server` instance. A second connect
//! is rejected with `AudioError::Busy` (`-EBUSY`); the rejection log
//! is rate-limited so a misbehaving client cannot flood the boot
//! console.
//!
//! Track D.1 lands the API shell. The behavioral tests + the real
//! rate-limited log path land in D.5.

#![allow(dead_code)] // D.5 consumes every symbol; see module docs.

/// Number of recent rejection log lines suppressed per second-attempt
/// window. The acceptance bullet says "logged once per second"; the
/// rate limiter tracks the last-log time per attempt and silences
/// repeats inside the window.
pub const REJECT_LOG_WINDOW_TICKS: u32 = 1;

/// Per-client admission state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientState {
    /// No client owns the slot.
    Idle,
    /// A client owns the slot. The `client_id` is the protocol-level
    /// identifier (the IPC reply label or socket fd) the io loop
    /// uses to route subsequent messages to the same `Stream`.
    Owned { client_id: u32 },
}

impl Default for ClientState {
    fn default() -> Self {
        Self::Idle
    }
}

/// At-most-one-client admission registry. Owned by the io loop;
/// `try_admit` is called on every incoming message header, `release`
/// is called on `Close` or socket disconnect.
pub struct ClientRegistry {
    pub(crate) state: ClientState,
    /// Counter of rejection events since the last log emission. The
    /// io loop is single-threaded so this is a plain `u32`.
    pub(crate) rejects_since_last_log: u32,
    /// Tick counter (call-site supplied) of the most recent rejection
    /// log emission. The rate limiter compares against the current
    /// tick to decide whether to log.
    pub(crate) last_log_tick: u32,
}

impl Default for ClientRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientRegistry {
    pub const fn new() -> Self {
        Self {
            state: ClientState::Idle,
            rejects_since_last_log: 0,
            last_log_tick: 0,
        }
    }

    /// Admit a client — D.5-red stub always rejects.  The real
    /// behavior lands in D.5-green.
    pub fn try_admit(&mut self, _client_id: u32) -> bool {
        false
    }

    /// Release a client — D.5-red stub no-op.
    pub fn release(&mut self, _client_id: u32) {}

    /// Rate-limited reject-log gate — D.5-red stub never emits.
    pub fn should_log_reject(&mut self, _current_tick: u32) -> bool {
        false
    }

    /// Snapshot of the current state.
    pub fn state(&self) -> ClientState {
        self.state
    }

    /// Number of rejection events since the last log emission.
    pub fn rejects_since_last_log(&self) -> u32 {
        self.rejects_since_last_log
    }
}

// ---------------------------------------------------------------------------
// Tests — D.5 host coverage
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_admit_succeeds() {
        let mut reg = ClientRegistry::new();
        assert!(reg.try_admit(1));
        assert_eq!(reg.state(), ClientState::Owned { client_id: 1 });
    }

    #[test]
    fn second_admit_with_different_id_is_rejected_with_busy() {
        // Acceptance: second connect rejected with `-EBUSY`.
        let mut reg = ClientRegistry::new();
        assert!(reg.try_admit(1));
        assert!(!reg.try_admit(2));
        // Slot still owned by the first admit.
        assert_eq!(reg.state(), ClientState::Owned { client_id: 1 });
    }

    #[test]
    fn second_admit_by_same_owner_is_idempotent() {
        // The same client re-asking for admission is not a rejection
        // — covers the protocol's "open after open" client-bug path.
        let mut reg = ClientRegistry::new();
        assert!(reg.try_admit(1));
        assert!(reg.try_admit(1));
        // No rejection counter advanced.
        assert_eq!(reg.rejects_since_last_log(), 0);
    }

    #[test]
    fn release_for_owner_returns_to_idle() {
        let mut reg = ClientRegistry::new();
        reg.try_admit(7);
        reg.release(7);
        assert_eq!(reg.state(), ClientState::Idle);
    }

    #[test]
    fn release_for_non_owner_is_noop() {
        // Disconnect of a non-owning client must not steal the slot.
        let mut reg = ClientRegistry::new();
        reg.try_admit(1);
        reg.release(2);
        assert_eq!(reg.state(), ClientState::Owned { client_id: 1 });
    }

    #[test]
    fn release_when_idle_is_noop() {
        let mut reg = ClientRegistry::new();
        reg.release(99);
        assert_eq!(reg.state(), ClientState::Idle);
    }

    #[test]
    fn after_release_next_admit_succeeds() {
        // Acceptance: disconnect releases the stream slot
        // synchronously; the next admit is admitted.
        let mut reg = ClientRegistry::new();
        reg.try_admit(1);
        reg.release(1);
        assert!(reg.try_admit(2));
        assert_eq!(reg.state(), ClientState::Owned { client_id: 2 });
    }

    // -- Rate-limited rejection logging ----------------------------------

    #[test]
    fn rejection_counter_advances_per_failed_admit() {
        let mut reg = ClientRegistry::new();
        reg.try_admit(1);
        reg.try_admit(2);
        reg.try_admit(3);
        reg.try_admit(4);
        assert_eq!(reg.rejects_since_last_log(), 3);
    }

    #[test]
    fn should_log_reject_returns_false_with_no_rejections() {
        let mut reg = ClientRegistry::new();
        reg.try_admit(1);
        // No rejections yet — should not log.
        assert!(!reg.should_log_reject(0));
        assert!(!reg.should_log_reject(100));
    }

    #[test]
    fn should_log_reject_returns_true_after_first_rejection() {
        let mut reg = ClientRegistry::new();
        reg.try_admit(1);
        reg.try_admit(2); // rejection
        assert!(reg.should_log_reject(0));
    }

    #[test]
    fn should_log_reject_resets_counter_on_emit() {
        let mut reg = ClientRegistry::new();
        reg.try_admit(1);
        reg.try_admit(2);
        reg.try_admit(3);
        assert_eq!(reg.rejects_since_last_log(), 2);
        // First should-log emits.
        assert!(reg.should_log_reject(0));
        // After emit the counter resets.
        assert_eq!(reg.rejects_since_last_log(), 0);
    }

    #[test]
    fn should_log_reject_suppresses_inside_window() {
        // Acceptance: rate-limited per second-attempt — at most one
        // log per `REJECT_LOG_WINDOW_TICKS` window.
        let mut reg = ClientRegistry::new();
        reg.try_admit(1);
        reg.try_admit(2); // first rejection
        assert!(reg.should_log_reject(0));
        reg.try_admit(3); // second rejection inside the window
        // Same tick — must NOT log.
        assert!(!reg.should_log_reject(0));
    }

    #[test]
    fn should_log_reject_emits_again_after_window_elapses() {
        let mut reg = ClientRegistry::new();
        reg.try_admit(1);
        reg.try_admit(2);
        assert!(reg.should_log_reject(0));
        reg.try_admit(3);
        // Window elapsed.
        assert!(reg.should_log_reject(REJECT_LOG_WINDOW_TICKS));
    }

    #[test]
    fn admit_and_release_does_not_allocate_per_dispatch() {
        // Acceptance: no allocation per dispatch. The registry has
        // no Vec / Box / String fields — exercising admit/release
        // many times must be O(1) memory.
        let mut reg = ClientRegistry::new();
        for i in 0..1000 {
            reg.try_admit(i);
            reg.release(i);
        }
        assert_eq!(reg.state(), ClientState::Idle);
    }

    #[test]
    fn reject_log_window_is_one_tick_per_acceptance() {
        // Acceptance bullet says "logged once per second"; the
        // call-site supplies seconds via `current_tick`, so the
        // window in ticks is 1.
        assert_eq!(REJECT_LOG_WINDOW_TICKS, 1);
    }
}
