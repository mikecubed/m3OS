//! Edge-triggered epoll readiness logic, host-testable.
//!
//! `sys_epoll_wait` in the kernel clones the interest list and computes fd
//! readiness *without* holding the epoll-table lock (so `fd_poll_events` can
//! descend into the net/pipe layers safely), then applies the result. The pure
//! decision — "given the current readiness, the requested event mask, and (for
//! edge-triggered interests) the last-reported readiness watermark, what should
//! be reported and what is the refreshed watermark?" — lives here so it can be
//! unit-tested on the host without a running kernel.
//!
//! Edge state is **per-interest**, never per-fd: the same fd may sit in two
//! epoll instances and must track its edges independently. The kernel stores
//! `last_ready` on each `EpollInterest` and threads it through this function.

/// Outcome of evaluating one epoll interest against current fd readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterestEval {
    /// The events to report to userspace (0 when nothing is reportable).
    pub revents: u32,
    /// Whether an event should be emitted for this interest this round.
    pub emit: bool,
    /// The refreshed edge-triggered watermark to store back on the interest.
    /// Meaningful only for edge-triggered interests; level-triggered callers
    /// may ignore it.
    pub new_last_ready: u32,
}

/// Evaluate one epoll interest.
///
/// * `ready` — current readiness bits from `fd_poll_events` (EPOLL*-compatible).
/// * `event_mask` — the requested event bits with control bits (EPOLLET /
///   EPOLLONESHOT) already stripped.
/// * `unconditional_mask` — bits always reported when ready even if not
///   requested (EPOLLHUP | EPOLLERR).
/// * `last_ready` — the interest's previous edge watermark (ignored when not ET).
/// * `is_et` — whether the interest is edge-triggered (`EPOLLET`).
///
/// Level-triggered interests emit whenever any requested or unconditional bit
/// is ready. Edge-triggered interests emit only on a not-ready→ready
/// transition: a bit that is ready now and was *not* in the watermark. The
/// watermark is always refreshed to the current `revents`, so draining an fd
/// lowers it and a later refill produces a fresh edge.
pub fn evaluate_interest(
    ready: u32,
    event_mask: u32,
    unconditional_mask: u32,
    last_ready: u32,
    is_et: bool,
) -> InterestEval {
    let revents = (ready & event_mask) | (ready & unconditional_mask);
    let emit = if is_et {
        (revents & !last_ready) != 0
    } else {
        revents != 0
    };
    InterestEval {
        revents,
        emit,
        new_last_ready: revents,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // EPOLL-compatible readiness bits (mirror the kernel's syscall module).
    const IN: u32 = 0x001;
    const OUT: u32 = 0x004;
    const ERR: u32 = 0x008;
    const HUP: u32 = 0x010;
    const RDHUP: u32 = 0x2000;
    const UNCOND: u32 = ERR | HUP;

    #[test]
    fn level_triggered_emits_every_time_while_ready() {
        // LT ignores the watermark: a still-readable fd re-reports each call.
        let e = evaluate_interest(IN, IN | OUT, UNCOND, /*last*/ IN, /*et*/ false);
        assert!(e.emit);
        assert_eq!(e.revents, IN);
    }

    #[test]
    fn level_triggered_silent_when_not_ready() {
        let e = evaluate_interest(0, IN | OUT, UNCOND, 0, false);
        assert!(!e.emit);
        assert_eq!(e.revents, 0);
    }

    #[test]
    fn edge_triggered_emits_on_first_edge() {
        let e = evaluate_interest(IN, IN | OUT, UNCOND, /*last*/ 0, /*et*/ true);
        assert!(e.emit);
        assert_eq!(e.revents, IN);
        assert_eq!(e.new_last_ready, IN);
    }

    #[test]
    fn edge_triggered_suppressed_when_already_reported() {
        // Still-readable, already at the watermark → no new edge, no emit.
        let e = evaluate_interest(IN, IN | OUT, UNCOND, /*last*/ IN, /*et*/ true);
        assert!(!e.emit);
        // Watermark stays put.
        assert_eq!(e.new_last_ready, IN);
    }

    #[test]
    fn edge_triggered_re_triggers_after_drain_then_refill() {
        // 1) first edge reports, watermark := IN.
        let e1 = evaluate_interest(IN, IN | OUT, UNCOND, 0, true);
        assert!(e1.emit);
        assert_eq!(e1.new_last_ready, IN);
        // 2) fd drained: ready drops to 0, no emit, watermark lowered to 0.
        let e2 = evaluate_interest(0, IN | OUT, UNCOND, e1.new_last_ready, true);
        assert!(!e2.emit);
        assert_eq!(e2.new_last_ready, 0);
        // 3) fd refilled: ready→IN against the lowered watermark = fresh edge.
        let e3 = evaluate_interest(IN, IN | OUT, UNCOND, e2.new_last_ready, true);
        assert!(e3.emit);
        assert_eq!(e3.revents, IN);
    }

    #[test]
    fn edge_triggered_new_bit_is_an_edge() {
        // Already reported readable; now also writable → OUT is a new edge.
        let e = evaluate_interest(IN | OUT, IN | OUT, UNCOND, /*last*/ IN, true);
        assert!(e.emit);
        assert_eq!(e.revents, IN | OUT);
        assert_eq!(e.new_last_ready, IN | OUT);
    }

    #[test]
    fn unconditional_hup_reported_even_when_not_requested() {
        // Caller only asked for IN, but HUP is always surfaced (level).
        let e = evaluate_interest(HUP, IN, UNCOND, 0, false);
        assert!(e.emit);
        assert_eq!(e.revents, HUP);
    }

    #[test]
    fn edge_triggered_hup_is_one_shot_until_cleared() {
        let e1 = evaluate_interest(HUP, IN, UNCOND, 0, true);
        assert!(e1.emit);
        assert_eq!(e1.revents, HUP);
        // Persistent HUP at the watermark → not re-reported under ET.
        let e2 = evaluate_interest(HUP, IN, UNCOND, e1.new_last_ready, true);
        assert!(!e2.emit);
    }

    #[test]
    fn rdhup_reported_only_when_requested() {
        // RDHUP is NOT unconditional: a caller that didn't request it (mask=IN)
        // never sees it even when the fd reports a half-close.
        let not_requested = evaluate_interest(IN | RDHUP, IN, UNCOND, 0, false);
        assert_eq!(not_requested.revents, IN);
        // A caller that requested RDHUP gets it.
        let requested = evaluate_interest(IN | RDHUP, IN | RDHUP, UNCOND, 0, false);
        assert_eq!(requested.revents, IN | RDHUP);
    }
}
