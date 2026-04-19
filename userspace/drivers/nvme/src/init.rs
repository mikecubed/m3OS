//! NVMe controller bring-up — Phase 55b Track D.2.
//!
//! This module ports the Phase 55 in-kernel controller bring-up
//! sequence (originally `kernel/src/blk/nvme.rs`) to the ring-3
//! `driver_runtime` HAL. Register layouts (`kernel_core::nvme::*`) stay;
//! the MMIO and DMA access wrappers move from `crate::pci::bar::MmioRegion`
//! / `crate::mm::dma::DmaBuffer` to [`driver_runtime::Mmio`] and
//! [`driver_runtime::DmaBuffer`].
//!
//! # Pure reset state machine
//!
//! The reset / enable / Identify sequence is a small finite automaton
//! that does not depend on real MMIO or DMA. Extracting it as
//! [`BringUpStateMachine`] gives D.2 a host-testable surface: tests feed
//! synthetic `CAP` / `CSTS` reads and assert the machine advances
//! through the states the NVMe spec requires. The real driver layers
//! MMIO I/O on top — every [`BringUpAction`] produced by the machine
//! turns into one of `Mmio::write_reg` / `Mmio::read_reg` /
//! `DmaBuffer::allocate` / a polled wait loop that reads `CSTS.RDY`.
//!
//! # State invariants
//!
//! - Start: [`BringUpState::ResetDisable`]. First action is to clear
//!   `CC.EN` if the firmware left it set.
//! - Terminal success: [`BringUpState::Identified`]. Controller is
//!   enabled, admin SQ/CQ are programmed, and Identify Controller +
//!   Identify Namespace have completed.
//! - Terminal failure: [`BringUpState::Failed`] carrying a
//!   [`BringUpError`]. The driver surfaces this to waiting IPC clients
//!   as [`kernel_core::driver_ipc::block::BlockDriverError::IoError`].
//!
//! The state machine never panics. Unexpected inputs (for example,
//! `observe_csts` called outside a wait state) are ignored so the
//! driver loop cannot accidentally advance the machine.

use core::fmt;

use kernel_core::nvme as knvme;

// ---------------------------------------------------------------------------
// Queue sizing constants — mirror Phase 55 D.1 `kernel/src/blk/nvme.rs`.
// ---------------------------------------------------------------------------

/// NVMe memory page size assumed for PRP arithmetic. NVMe §4.3 allows
/// `2^(12 + CAP.MPSMIN)` pages; every target we care about reports
/// `MPSMIN == 0`, so 4 KiB is the correct value. Keeping the constant
/// in one place guarantees the PRP math and the DMA buffer sizes never
/// drift.
pub const NVME_PAGE_BYTES: usize = 4096;

/// Admin queue depth. 64 is well above the 2-3 commands admin bring-up
/// ever has in flight and stays far below every `CAP.MQES` we expect
/// on real controllers.
pub const ADMIN_QUEUE_DEPTH: usize = 64;

/// Safety margin on top of `CAP.TO * 500 ms` before the reset / enable
/// wait treats the controller as wedged. Matches Phase 55 D.1's
/// `RESET_SAFETY_MARGIN_TICKS`.
pub const RESET_SAFETY_MARGIN_MS: u64 = 1_000;

// ---------------------------------------------------------------------------
// BringUpError
// ---------------------------------------------------------------------------

/// Reason a controller bring-up attempt failed.
///
/// Each variant is data — the driver surfaces it to IPC clients as
/// `BlockDriverError::IoError` and logs the specific variant for
/// post-mortem. No variant ever triggers a panic path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BringUpError {
    /// Controller did not advertise the NVM command set (`CAP.CSS`
    /// bit 37 was clear). A future phase may add NVMe-MI support.
    NvmNotAdvertised,
    /// `CSTS.RDY` did not clear after clearing `CC.EN` within the
    /// `CAP.TO` + safety-margin budget.
    ResetTimeout,
    /// `CSTS.RDY` did not set after writing `CC.EN=1` within the
    /// budget.
    EnableTimeout,
    /// `CSTS.CFS` was observed during bring-up. The controller needs
    /// a full subsystem reset before it is usable again.
    ControllerFatal,
    /// An admin command (Identify Controller / Identify Namespace /
    /// Create I/O queue) failed with non-zero status, or timed out.
    AdminCommandFailed,
    /// BAR0 is smaller than the doorbell range requires; bring-up
    /// aborted before any MMIO programming.
    BarTooSmall,
    /// `CAP.MQES + 1` was smaller than 2, leaving no room for the
    /// admin queue.
    QueueTooShallow,
}

impl fmt::Display for BringUpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NvmNotAdvertised => f.write_str("controller did not advertise NVM command set"),
            Self::ResetTimeout => f.write_str("timeout waiting for CSTS.RDY=0 during reset"),
            Self::EnableTimeout => f.write_str("timeout waiting for CSTS.RDY=1 during enable"),
            Self::ControllerFatal => f.write_str("controller reported CSTS.CFS fatal status"),
            Self::AdminCommandFailed => f.write_str("admin command failed or timed out"),
            Self::BarTooSmall => f.write_str("BAR0 too small for NVMe doorbell range"),
            Self::QueueTooShallow => f.write_str("CAP.MQES too small for any admin queue"),
        }
    }
}

// ---------------------------------------------------------------------------
// BringUpState / BringUpAction
// ---------------------------------------------------------------------------

/// State within the controller bring-up sequence. Ordering mirrors the
/// NVMe §3.5.1 "Controller Initialization" sequence with the Identify
/// post-conditions appended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BringUpState {
    /// Initial state. Driver must clear `CC.EN` if set.
    ResetDisable,
    /// Waiting for `CSTS.RDY` to clear after `CC.EN = 0`.
    ResetWait,
    /// Controller is disabled. Next: allocate admin SQ/CQ, program
    /// `AQA`, `ASQ`, `ACQ`.
    ProgramAdminQueue,
    /// Admin registers programmed. Next: write `CC` with `EN = 1`.
    EnableController,
    /// Waiting for `CSTS.RDY` to set after `CC.EN = 1`.
    EnableWait,
    /// Controller enabled. Next: Identify Controller (CNS=0x01).
    IdentifyController,
    /// Identify Controller done. Next: Identify Namespace (CNS=0x00).
    IdentifyNamespace,
    /// Bring-up complete. I/O queue pair setup is Track D.3.
    Identified,
    /// Terminal failure.
    Failed(BringUpError),
}

/// External action the driver must carry out for the current state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BringUpAction {
    /// Write `CC & !CC_EN` to clear the enable bit.
    WriteCcDisable,
    /// Poll `CSTS` until `CSTS.RDY == 0` or the budget expires.
    AwaitCstsReset,
    /// Allocate admin SQ / CQ and program `AQA`, `ASQ`, `ACQ`.
    ProgramAdminRegisters,
    /// Write `CC` with `EN = 1` and correct IOSQES / IOCQES.
    WriteCcEnable,
    /// Poll `CSTS` until `CSTS.RDY == 1` or the budget expires.
    AwaitCstsReady,
    /// Submit Identify Controller (CNS=0x01).
    SubmitIdentifyController,
    /// Submit Identify Namespace (CNS=0x00) for the selected NSID.
    SubmitIdentifyNamespace,
    /// No further action — bring-up is in a terminal state.
    Idle,
}

// ---------------------------------------------------------------------------
// BringUpStateMachine — pure-logic controller bring-up driver.
// ---------------------------------------------------------------------------

/// Pure-logic controller bring-up driver.
///
/// Construct with [`BringUpStateMachine::new`]; feed register-read
/// events via the `observe_*` methods; call
/// [`BringUpStateMachine::next_action`] to learn what to do. The
/// machine does not issue MMIO / DMA itself — the driver loop carries
/// out each [`BringUpAction`] and reports the resulting observation
/// back.
#[derive(Clone, Debug)]
pub struct BringUpStateMachine {
    state: BringUpState,
    reset_budget_ms: u64,
    cap: knvme::NvmeCap,
}

impl BringUpStateMachine {
    /// Build a bring-up state machine from the controller's `CAP`
    /// register value.
    pub fn new(cap: knvme::NvmeCap) -> Result<Self, BringUpError> {
        if !cap.css_nvme() {
            return Err(BringUpError::NvmNotAdvertised);
        }
        if cap.mqes() < 2 {
            return Err(BringUpError::QueueTooShallow);
        }
        Ok(Self {
            state: BringUpState::ResetDisable,
            reset_budget_ms: reset_budget_ms(cap.timeout_500ms_units()),
            cap,
        })
    }

    /// Current state. Used by the driver loop to decide which action
    /// to issue and by tests to assert transitions.
    pub fn state(&self) -> BringUpState {
        self.state
    }

    /// Polling budget in milliseconds — the driver's wait loop uses
    /// this to bound `AwaitCstsReset` / `AwaitCstsReady` waits.
    pub fn reset_budget_ms(&self) -> u64 {
        self.reset_budget_ms
    }

    /// `CAP.MQES + 1` — upper bound on queue entries the hardware
    /// accepts. The driver clamps its configured `ADMIN_QUEUE_DEPTH`
    /// against this.
    pub fn max_queue_entries(&self) -> u16 {
        self.cap.mqes()
    }

    /// Doorbell stride in bytes, encoded as `4 << CAP.DSTRD`.
    pub fn doorbell_stride_bytes(&self) -> usize {
        self.cap.doorbell_stride()
    }

    /// Action the driver should perform next, given the current
    /// state.
    pub fn next_action(&self) -> BringUpAction {
        match self.state {
            BringUpState::ResetDisable => BringUpAction::WriteCcDisable,
            BringUpState::ResetWait => BringUpAction::AwaitCstsReset,
            BringUpState::ProgramAdminQueue => BringUpAction::ProgramAdminRegisters,
            BringUpState::EnableController => BringUpAction::WriteCcEnable,
            BringUpState::EnableWait => BringUpAction::AwaitCstsReady,
            BringUpState::IdentifyController => BringUpAction::SubmitIdentifyController,
            BringUpState::IdentifyNamespace => BringUpAction::SubmitIdentifyNamespace,
            BringUpState::Identified | BringUpState::Failed(_) => BringUpAction::Idle,
        }
    }

    /// Notify the machine that the driver has written `CC` with
    /// `EN = 0`. Transitions `ResetDisable` → `ResetWait`.
    pub fn notify_cc_disabled(&mut self) {
        if matches!(self.state, BringUpState::ResetDisable) {
            self.state = BringUpState::ResetWait;
        }
    }

    /// Feed a `CSTS` observation. Handles both `ResetWait` (clear bit
    /// advances) and `EnableWait` (set bit advances). `CSTS.CFS`
    /// short-circuits to [`BringUpError::ControllerFatal`].
    pub fn observe_csts(&mut self, csts: u32) {
        match self.state {
            BringUpState::ResetWait => {
                if csts & knvme::CSTS_CFS != 0 {
                    self.state = BringUpState::Failed(BringUpError::ControllerFatal);
                } else if csts & knvme::CSTS_RDY == 0 {
                    self.state = BringUpState::ProgramAdminQueue;
                }
            }
            BringUpState::EnableWait => {
                if csts & knvme::CSTS_CFS != 0 {
                    self.state = BringUpState::Failed(BringUpError::ControllerFatal);
                } else if csts & knvme::CSTS_RDY != 0 {
                    self.state = BringUpState::IdentifyController;
                }
            }
            _ => {
                // Observations outside a wait state are ignored so
                // the driver cannot accidentally advance the machine
                // with a stale read.
            }
        }
    }

    /// Signal that the `AwaitCstsReset` / `AwaitCstsReady` budget
    /// expired without observing the expected bit transition.
    pub fn timeout(&mut self) {
        match self.state {
            BringUpState::ResetWait => {
                self.state = BringUpState::Failed(BringUpError::ResetTimeout);
            }
            BringUpState::EnableWait => {
                self.state = BringUpState::Failed(BringUpError::EnableTimeout);
            }
            _ => {}
        }
    }

    /// Notify the machine that `AQA`/`ASQ`/`ACQ` have been programmed
    /// and the admin SQ / CQ are ready.
    pub fn notify_admin_programmed(&mut self) {
        if matches!(self.state, BringUpState::ProgramAdminQueue) {
            self.state = BringUpState::EnableController;
        }
    }

    /// Notify the machine that `CC` was written with `EN = 1`.
    pub fn notify_cc_enabled(&mut self) {
        if matches!(self.state, BringUpState::EnableController) {
            self.state = BringUpState::EnableWait;
        }
    }

    /// Report the outcome of the Identify Controller admin command.
    /// `status_code == 0` advances to
    /// [`BringUpState::IdentifyNamespace`]; any non-zero status lands
    /// in [`BringUpError::AdminCommandFailed`].
    pub fn notify_identify_controller(&mut self, status_code: u16) {
        if matches!(self.state, BringUpState::IdentifyController) {
            if status_code == 0 {
                self.state = BringUpState::IdentifyNamespace;
            } else {
                self.state = BringUpState::Failed(BringUpError::AdminCommandFailed);
            }
        }
    }

    /// Report the outcome of the Identify Namespace admin command.
    /// `status_code == 0` finishes bring-up.
    pub fn notify_identify_namespace(&mut self, status_code: u16) {
        if matches!(self.state, BringUpState::IdentifyNamespace) {
            if status_code == 0 {
                self.state = BringUpState::Identified;
            } else {
                self.state = BringUpState::Failed(BringUpError::AdminCommandFailed);
            }
        }
    }

    /// True when the machine has observed the Identify Namespace
    /// success transition.
    pub fn is_complete(&self) -> bool {
        matches!(self.state, BringUpState::Identified)
    }

    /// True when the machine is in any terminal state (success or
    /// failure). Driver loop exits on this.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            BringUpState::Identified | BringUpState::Failed(_)
        )
    }

    /// Error variant when in a terminal failure state. `None`
    /// otherwise.
    pub fn error(&self) -> Option<BringUpError> {
        match self.state {
            BringUpState::Failed(e) => Some(e),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Encoders — byte-for-byte matches with Phase 55 D.1.
// ---------------------------------------------------------------------------

/// Compute the reset / enable polling window in milliseconds from
/// `CAP.TO` (in 500-ms units). Mirrors the Phase 55 D.1 helper but
/// returns milliseconds directly so the userspace driver can feed it
/// into its own bounded-wait loop.
pub fn reset_budget_ms(to_500ms_units: u8) -> u64 {
    let units = to_500ms_units.max(1) as u64;
    units
        .saturating_mul(500)
        .saturating_add(RESET_SAFETY_MARGIN_MS)
}

/// Encode `AQA` from the admin queue depth. `ASQS` (bits 11:0) and
/// `ACQS` (bits 27:16) both hold `entries - 1` per NVMe §3.1.9.
pub fn encode_aqa(entries: u16) -> u32 {
    let qsize = entries.saturating_sub(1) as u32;
    (qsize & 0x0FFF) | ((qsize & 0x0FFF) << 16)
}

/// Value to write into `CC` when enabling the controller. `IOSQES = 6`
/// (64-byte SQ entries), `IOCQES = 4` (16-byte CQ entries), `MPS = 0`
/// (4 KiB), `AMS = 0`, `CSS = 0` (NVM), `SHN = 0`, `EN = 1`. Exactly
/// the bits Phase 55 D.1's `enable_controller` writes.
pub fn encode_cc_enable() -> u32 {
    (6u32 << knvme::CC_IOSQES_SHIFT)
        | (4u32 << knvme::CC_IOCQES_SHIFT)
        | (0u32 << knvme::CC_MPS_SHIFT)
        | (0u32 << knvme::CC_AMS_SHIFT)
        | (0u32 << knvme::CC_CSS_SHIFT)
        | (0u32 << knvme::CC_SHN_SHIFT)
        | knvme::CC_EN
}

// ---------------------------------------------------------------------------
// Tests — D.2 acceptance. Every test here pins a spec-level behavior
// the driver loop depends on.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_cap() -> knvme::NvmeCap {
        let mut cap = 0u64;
        cap |= 0x00FF; // MQES raw 255 → mqes() = 256
        cap |= 1 << 16; // CQR
        cap |= 0x20u64 << 24; // TO = 0x20 (16 s)
        cap |= 0u64 << 32; // DSTRD = 0 → 4-byte stride
        cap |= 1u64 << 37; // CSS.NVM
        cap |= 0u64 << 48; // MPSMIN = 0 → 4 KiB
        knvme::NvmeCap(cap)
    }

    // Construction / preconditions ---------------------------------

    #[test]
    fn new_rejects_controller_without_nvm_command_set() {
        let raw = default_cap().0 & !(1u64 << 37);
        let err = BringUpStateMachine::new(knvme::NvmeCap(raw)).expect_err("non-NVM must fail");
        assert_eq!(err, BringUpError::NvmNotAdvertised);
    }

    #[test]
    fn new_rejects_mqes_too_small_for_admin_queue() {
        let raw = default_cap().0 & !0xFFFFu64;
        let err = BringUpStateMachine::new(knvme::NvmeCap(raw)).expect_err("tiny mqes must fail");
        assert_eq!(err, BringUpError::QueueTooShallow);
    }

    #[test]
    fn new_succeeds_with_sane_cap() {
        let sm = BringUpStateMachine::new(default_cap()).expect("valid cap");
        assert_eq!(sm.state(), BringUpState::ResetDisable);
        assert_eq!(sm.max_queue_entries(), 256);
        assert_eq!(sm.doorbell_stride_bytes(), 4);
        assert_eq!(sm.reset_budget_ms(), 0x20 * 500 + 1_000);
    }

    // Happy path ---------------------------------------------------

    #[test]
    fn happy_path_drives_sequence_to_identified() {
        let mut sm = BringUpStateMachine::new(default_cap()).unwrap();
        assert_eq!(sm.next_action(), BringUpAction::WriteCcDisable);

        sm.notify_cc_disabled();
        assert_eq!(sm.state(), BringUpState::ResetWait);
        assert_eq!(sm.next_action(), BringUpAction::AwaitCstsReset);

        sm.observe_csts(knvme::CSTS_RDY);
        assert_eq!(sm.state(), BringUpState::ResetWait);
        sm.observe_csts(0);
        assert_eq!(sm.state(), BringUpState::ProgramAdminQueue);
        assert_eq!(sm.next_action(), BringUpAction::ProgramAdminRegisters);

        sm.notify_admin_programmed();
        assert_eq!(sm.state(), BringUpState::EnableController);
        assert_eq!(sm.next_action(), BringUpAction::WriteCcEnable);

        sm.notify_cc_enabled();
        assert_eq!(sm.state(), BringUpState::EnableWait);
        assert_eq!(sm.next_action(), BringUpAction::AwaitCstsReady);

        sm.observe_csts(knvme::CSTS_RDY);
        assert_eq!(sm.state(), BringUpState::IdentifyController);
        assert_eq!(sm.next_action(), BringUpAction::SubmitIdentifyController);

        sm.notify_identify_controller(0);
        assert_eq!(sm.state(), BringUpState::IdentifyNamespace);
        assert_eq!(sm.next_action(), BringUpAction::SubmitIdentifyNamespace);

        sm.notify_identify_namespace(0);
        assert_eq!(sm.state(), BringUpState::Identified);
        assert!(sm.is_complete());
        assert!(sm.is_terminal());
        assert_eq!(sm.error(), None);
        assert_eq!(sm.next_action(), BringUpAction::Idle);
    }

    // Failure transitions ------------------------------------------

    #[test]
    fn reset_timeout_lands_in_failed_state() {
        let mut sm = BringUpStateMachine::new(default_cap()).unwrap();
        sm.notify_cc_disabled();
        sm.timeout();
        assert!(sm.is_terminal());
        assert_eq!(sm.error(), Some(BringUpError::ResetTimeout));
    }

    #[test]
    fn enable_timeout_lands_in_failed_state() {
        let mut sm = BringUpStateMachine::new(default_cap()).unwrap();
        sm.notify_cc_disabled();
        sm.observe_csts(0);
        sm.notify_admin_programmed();
        sm.notify_cc_enabled();
        sm.timeout();
        assert!(sm.is_terminal());
        assert_eq!(sm.error(), Some(BringUpError::EnableTimeout));
    }

    #[test]
    fn csts_cfs_during_reset_short_circuits_to_controller_fatal() {
        let mut sm = BringUpStateMachine::new(default_cap()).unwrap();
        sm.notify_cc_disabled();
        sm.observe_csts(knvme::CSTS_CFS);
        assert_eq!(sm.error(), Some(BringUpError::ControllerFatal));
    }

    #[test]
    fn csts_cfs_during_enable_short_circuits_to_controller_fatal() {
        let mut sm = BringUpStateMachine::new(default_cap()).unwrap();
        sm.notify_cc_disabled();
        sm.observe_csts(0);
        sm.notify_admin_programmed();
        sm.notify_cc_enabled();
        sm.observe_csts(knvme::CSTS_CFS | knvme::CSTS_RDY);
        assert_eq!(sm.error(), Some(BringUpError::ControllerFatal));
    }

    #[test]
    fn identify_controller_failure_reports_admin_command_failed() {
        let mut sm = BringUpStateMachine::new(default_cap()).unwrap();
        sm.notify_cc_disabled();
        sm.observe_csts(0);
        sm.notify_admin_programmed();
        sm.notify_cc_enabled();
        sm.observe_csts(knvme::CSTS_RDY);
        sm.notify_identify_controller(0x81);
        assert_eq!(sm.error(), Some(BringUpError::AdminCommandFailed));
    }

    #[test]
    fn identify_namespace_failure_reports_admin_command_failed() {
        let mut sm = BringUpStateMachine::new(default_cap()).unwrap();
        sm.notify_cc_disabled();
        sm.observe_csts(0);
        sm.notify_admin_programmed();
        sm.notify_cc_enabled();
        sm.observe_csts(knvme::CSTS_RDY);
        sm.notify_identify_controller(0);
        sm.notify_identify_namespace(0x42);
        assert_eq!(sm.error(), Some(BringUpError::AdminCommandFailed));
    }

    // Regression guards --------------------------------------------

    #[test]
    fn observe_csts_outside_wait_states_is_ignored() {
        let mut sm = BringUpStateMachine::new(default_cap()).unwrap();
        sm.observe_csts(knvme::CSTS_RDY);
        assert_eq!(sm.state(), BringUpState::ResetDisable);

        sm.notify_cc_disabled();
        sm.observe_csts(0);
        sm.notify_admin_programmed();
        sm.notify_cc_enabled();
        sm.observe_csts(knvme::CSTS_RDY);
        sm.notify_identify_controller(0);
        sm.notify_identify_namespace(0);
        sm.observe_csts(knvme::CSTS_CFS);
        assert!(sm.is_complete());
    }

    #[test]
    fn timeout_outside_wait_states_is_ignored() {
        let mut sm = BringUpStateMachine::new(default_cap()).unwrap();
        sm.timeout();
        assert_eq!(sm.state(), BringUpState::ResetDisable);
    }

    // Encoders ----------------------------------------------------

    #[test]
    fn encode_aqa_packs_asqs_and_acqs_with_entries_minus_one() {
        assert_eq!(encode_aqa(64), 63u32 | (63u32 << 16));
        assert_eq!(encode_aqa(1), 0);
        assert_eq!(encode_aqa(0), 0);
    }

    #[test]
    fn encode_cc_enable_byte_for_byte_matches_phase_55() {
        let expected =
            (6u32 << knvme::CC_IOSQES_SHIFT) | (4u32 << knvme::CC_IOCQES_SHIFT) | knvme::CC_EN;
        assert_eq!(encode_cc_enable(), expected);
    }

    #[test]
    fn reset_budget_ms_matches_phase_55_d1() {
        assert_eq!(reset_budget_ms(0), 500 + 1_000);
        assert_eq!(reset_budget_ms(4), 3_000);
    }
}
