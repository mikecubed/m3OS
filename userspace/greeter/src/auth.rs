//! Phase 71 Track D — authentication loop with backoff.
//!
//! Pure-logic state machine: feed in attempt results, get back either
//! "go" or "wait N ms". The real binary calls `passwd_lib::verify`
//! (via the `syscall_lib::sha256::verify_password` path) and the
//! `/etc/passwd` lookup to satisfy the [`AuthBackend`] trait. Tests
//! substitute a mock backend.

use alloc::string::String;

/// Phase 48 trust-floor: how many consecutive failures trigger
/// [`AuthOutcome::Backoff`].
pub const FAILURE_THRESHOLD: u32 = 3;
/// Phase 48 trust-floor: how long to wait before re-prompting after
/// the threshold is reached.
pub const BACKOFF_DURATION_SECS: u64 = 5;

/// Session descriptor handed off from greeter to `session_manager`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDescriptor {
    pub uid: u32,
    pub gid: u32,
    pub home: String,
    pub shell: String,
}

/// Authentication backend abstraction. Production binds it to
/// `passwd_lib::verify`; host tests substitute an in-memory fake.
pub trait AuthBackend {
    /// Verify `password` against the named user. Returns the user's
    /// session descriptor on success.
    fn verify(&self, username: &str, password: &str) -> Result<SessionDescriptor, AuthError>;
}

/// Typed authentication errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// Username does not exist in `/etc/passwd`.
    UnknownUser,
    /// Password hash did not match the stored entry.
    BadPassword,
    /// Account is locked (shadow field is `!` or `*`).
    AccountLocked,
    /// Could not read passwd / shadow file.
    StoreUnavailable,
}

/// Outcome of one [`AuthLoopState::record_attempt`] call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Authentication succeeded; greeter should emit the descriptor and exit.
    Success(SessionDescriptor),
    /// Auth failed but the failure budget is still under threshold.
    /// Re-prompt without delay.
    Failed(AuthError),
    /// Auth failed and threshold was reached. The caller must wait
    /// `wait_secs` seconds before re-prompting; on the next attempt
    /// the counter has been reset.
    Backoff { wait_secs: u64, reason: AuthError },
}

/// Stateful loop driver. Owns the consecutive-failure counter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthLoopState {
    consecutive_failures: u32,
}

impl AuthLoopState {
    pub const fn new() -> Self {
        Self {
            consecutive_failures: 0,
        }
    }

    /// Current failure counter — for tests + the per-iteration log line.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Reset the failure counter (e.g. after the backoff sleep returns).
    pub fn reset(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Record the result of one verify call and return the next
    /// action for the loop.
    pub fn record_attempt(&mut self, result: Result<SessionDescriptor, AuthError>) -> AuthOutcome {
        match result {
            Ok(desc) => {
                self.consecutive_failures = 0;
                AuthOutcome::Success(desc)
            }
            Err(err) => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                if self.consecutive_failures >= FAILURE_THRESHOLD {
                    let wait_secs = BACKOFF_DURATION_SECS;
                    // Reset *after* delivering the backoff so the next
                    // attempt starts with a clean counter.
                    self.consecutive_failures = 0;
                    AuthOutcome::Backoff {
                        wait_secs,
                        reason: err,
                    }
                } else {
                    AuthOutcome::Failed(err)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc() -> SessionDescriptor {
        SessionDescriptor {
            uid: 1000,
            gid: 1000,
            home: String::from("/home/u"),
            shell: String::from("/bin/sh0"),
        }
    }

    #[test]
    fn success_resets_counter() {
        let mut s = AuthLoopState::new();
        let _ = s.record_attempt(Err(AuthError::BadPassword));
        let _ = s.record_attempt(Err(AuthError::BadPassword));
        let out = s.record_attempt(Ok(desc()));
        assert!(matches!(out, AuthOutcome::Success(_)));
        assert_eq!(s.consecutive_failures(), 0);
    }

    #[test]
    fn three_failures_trigger_backoff_and_reset() {
        let mut s = AuthLoopState::new();
        assert!(matches!(
            s.record_attempt(Err(AuthError::BadPassword)),
            AuthOutcome::Failed(AuthError::BadPassword)
        ));
        assert!(matches!(
            s.record_attempt(Err(AuthError::BadPassword)),
            AuthOutcome::Failed(AuthError::BadPassword)
        ));
        let out = s.record_attempt(Err(AuthError::BadPassword));
        match out {
            AuthOutcome::Backoff { wait_secs, .. } => {
                assert_eq!(wait_secs, BACKOFF_DURATION_SECS);
            }
            other => panic!("expected Backoff, got {other:?}"),
        }
        // Counter resets after backoff so the next loop iteration
        // starts fresh.
        assert_eq!(s.consecutive_failures(), 0);
    }

    #[test]
    fn unknown_user_counted_same_as_bad_password() {
        let mut s = AuthLoopState::new();
        let _ = s.record_attempt(Err(AuthError::UnknownUser));
        let _ = s.record_attempt(Err(AuthError::UnknownUser));
        let out = s.record_attempt(Err(AuthError::UnknownUser));
        assert!(matches!(out, AuthOutcome::Backoff { .. }));
    }

    /// Mock backend used by the integration test below. Returns
    /// `Ok(desc)` only when `(username, password)` matches.
    struct FixedBackend;
    impl AuthBackend for FixedBackend {
        fn verify(&self, username: &str, password: &str) -> Result<SessionDescriptor, AuthError> {
            if username == "alice" && password == "secret" {
                Ok(desc())
            } else if username == "alice" {
                Err(AuthError::BadPassword)
            } else {
                Err(AuthError::UnknownUser)
            }
        }
    }

    #[test]
    fn integrated_loop_succeeds_after_two_failures() {
        let backend = FixedBackend;
        let mut s = AuthLoopState::new();
        let _ = s.record_attempt(backend.verify("alice", "wrong"));
        let _ = s.record_attempt(backend.verify("bob", "secret"));
        let out = s.record_attempt(backend.verify("alice", "secret"));
        assert!(matches!(out, AuthOutcome::Success(_)));
        assert_eq!(s.consecutive_failures(), 0);
    }
}
