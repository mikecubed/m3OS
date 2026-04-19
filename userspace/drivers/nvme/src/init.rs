//! NVMe controller bring-up — Phase 55b Track D.2 (Red commit).
//!
//! Track D.2 red. Declares the pure-logic state-machine surface the
//! driver loop will consume (`BringUpStateMachine`, `BringUpState`,
//! `BringUpAction`, `BringUpError`) and pins the behavior the green
//! commit must land. The stub here deliberately does *nothing* on
//! every transition so the host-testable tests below land red.
//!
//! The register layouts (`kernel_core::nvme::*`) stay; the MMIO and
//! DMA access wrappers move from `crate::pci::bar::MmioRegion` /
//! `crate::mm::dma::DmaBuffer` to [`driver_runtime::Mmio`] and
//! [`driver_runtime::DmaBuffer`] once the green commit lands.

use core::fmt;

use kernel_core::nvme as knvme;

/// NVMe memory page size assumed for PRP arithmetic (matches Phase 55
/// D.1 `kernel/src/blk/nvme.rs::NVME_PAGE_BYTES`).
pub const NVME_PAGE_BYTES: usize = 4096;

/// Admin queue depth (mirrors Phase 55 D.1 `ADMIN_QUEUE_ENTRIES`).
pub const ADMIN_QUEUE_DEPTH: usize = 64;

/// Safety margin on top of `CAP.TO * 500 ms` before the wait loop
/// treats the controller as wedged.
pub const RESET_SAFETY_MARGIN_MS: u64 = 1_000;

// ---------------------------------------------------------------------------
// BringUpError
// ---------------------------------------------------------------------------

/// Reason a controller bring-up attempt failed. Every variant is data;
/// no variant triggers a panic path. The driver collapses these to
/// [`kernel_core::driver_ipc::block::BlockDriverError::IoError`] when
/// replying to IPC clients.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BringUpError {
    NvmNotAdvertised,
    ResetTimeout,
    EnableTimeout,
    ControllerFatal,
    AdminCommandFailed,
    BarTooSmall,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BringUpState {
    ResetDisable,
    ResetWait,
    ProgramAdminQueue,
    EnableController,
    EnableWait,
    IdentifyController,
    IdentifyNamespace,
    Identified,
    Failed(BringUpError),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BringUpAction {
    WriteCcDisable,
    AwaitCstsReset,
    ProgramAdminRegisters,
    WriteCcEnable,
    AwaitCstsReady,
    SubmitIdentifyController,
    SubmitIdentifyNamespace,
    Idle,
}

// ---------------------------------------------------------------------------
// BringUpStateMachine — Red stub. Every transition is a no-op so tests
// below fail. The green commit replaces each method body with the
// logic documented on the state variants.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct BringUpStateMachine {
    #[allow(dead_code)]
    cap: knvme::NvmeCap,
}

impl BringUpStateMachine {
    pub fn new(_cap: knvme::NvmeCap) -> Result<Self, BringUpError> {
        // Red stub: always succeed so tests that exercise the failure
        // paths catch the missing validation.
        Err(BringUpError::AdminCommandFailed)
    }

    pub fn state(&self) -> BringUpState {
        BringUpState::ResetDisable
    }

    pub fn reset_budget_ms(&self) -> u64 {
        0
    }

    pub fn max_queue_entries(&self) -> u16 {
        0
    }

    pub fn doorbell_stride_bytes(&self) -> usize {
        0
    }

    pub fn next_action(&self) -> BringUpAction {
        BringUpAction::Idle
    }

    pub fn notify_cc_disabled(&mut self) {}
    pub fn observe_csts(&mut self, _csts: u32) {}
    pub fn timeout(&mut self) {}
    pub fn notify_admin_programmed(&mut self) {}
    pub fn notify_cc_enabled(&mut self) {}
    pub fn notify_identify_controller(&mut self, _status_code: u16) {}
    pub fn notify_identify_namespace(&mut self, _status_code: u16) {}
    pub fn is_complete(&self) -> bool {
        false
    }
    pub fn is_terminal(&self) -> bool {
        false
    }
    pub fn error(&self) -> Option<BringUpError> {
        None
    }
}

// ---------------------------------------------------------------------------
// Helpers — Red stubs.
// ---------------------------------------------------------------------------

pub fn reset_budget_ms(_to_500ms_units: u8) -> u64 {
    0
}

pub fn encode_aqa(_entries: u16) -> u32 {
    0
}

pub fn encode_cc_enable() -> u32 {
    0
}

// ---------------------------------------------------------------------------
// Tests — red. Every test here must land green after the D.2
// implementation commit.
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
