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

    /// Admit a client into the single slot.
    ///
    /// Returns `true` when the slot was empty (or the same client is
    /// re-asking) and the admission succeeded; returns `false` when
    /// another client owns the slot. The caller logs the rejection
    /// using [`Self::should_log_reject`].
    pub fn try_admit(&mut self, client_id: u32) -> bool {
        match self.state {
            ClientState::Idle => {
                self.state = ClientState::Owned { client_id };
                true
            }
            ClientState::Owned { client_id: owner } if owner == client_id => true,
            ClientState::Owned { .. } => {
                self.rejects_since_last_log = self.rejects_since_last_log.saturating_add(1);
                false
            }
        }
    }

    /// Release the slot. Idempotent — calling `release` on an already-
    /// idle registry is a no-op so socket-disconnect paths can call
    /// it without first probing the state.
    pub fn release(&mut self, client_id: u32) {
        if let ClientState::Owned { client_id: owner } = self.state {
            if owner == client_id {
                self.state = ClientState::Idle;
            }
        }
    }

    /// Returns `true` when the caller should emit a rejection log line
    /// for the most-recent failed `try_admit`.
    ///
    /// `current_tick` is a call-site monotonic counter (e.g.,
    /// elapsed-seconds). The rate limiter uses
    /// [`REJECT_LOG_WINDOW_TICKS`] as the suppression window.
    pub fn should_log_reject(&mut self, current_tick: u32) -> bool {
        if self.rejects_since_last_log == 0 {
            return false;
        }
        let elapsed = current_tick.wrapping_sub(self.last_log_tick);
        if elapsed >= REJECT_LOG_WINDOW_TICKS {
            self.last_log_tick = current_tick;
            self.rejects_since_last_log = 0;
            true
        } else {
            false
        }
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
// Tests — D.5 host coverage (lands red in next commit)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // D.5 lands the failing-test commit. Track D.1 keeps `#[cfg(test)]`
    // compiling green so the scaffold ships.
}
