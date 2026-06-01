//! r8125 V2-interrupt RX/TX servicing loop (Track D.1).
//!
//! The 8125 reuses the r8169 OWN-bit/TxPoll ring + Cfg9346 bring-up (it is a
//! second-generation r8169 MAC), so this module imports `r8169_driver`'s `Nic`
//! and ring/TX helpers and only swaps the **interrupt block**: the 8125 uses the
//! 32-bit V2 registers (`interrupt::{mask_all_v2,arm_v2,ack_v2}`) instead of the
//! classic 16-bit IMR/ISR. The version-branch decision (`interrupt::uses_v2`) is
//! host-tested.
//!
//! This module is os-binary only (it pulls in `syscall_lib` and the r8169
//! hardware modules, which build only for the bare-metal target).

extern crate alloc;

use driver_runtime::ipc::EndpointCap;
use driver_runtime::ipc::net::{NetReply, NetServer};
use driver_runtime::{DeviceCapHandle, IrqNotification, SyscallBackend as IrqSyscallBackend};
use kernel_core::driver_ipc::net::{NetDriverError, NetLinkEvent};

use r8169_hal::init::{Nic, R8169Regs};
use r8169_hal::io::{drain_rx, send_frame};

use crate::interrupt::{self, V2MmioOps};

/// Orphan-rule-safe view of the device handle as a `DeviceCapHandle`.
struct DeviceCapView {
    cap: u32,
}
impl DeviceCapHandle for DeviceCapView {
    fn cap_handle(&self) -> u32 {
        self.cap
    }
}

/// Bridge the r8169 `Mmio<R8169Regs>` window to the V2 interrupt trait so the
/// host-tested `interrupt` sequences can drive the real BAR.
struct V2Bridge<'a> {
    mmio: &'a driver_runtime::Mmio<R8169Regs>,
}
impl V2MmioOps for V2Bridge<'_> {
    fn read32(&self, offset: usize) -> u32 {
        self.mmio.read_reg::<u32>(offset)
    }
    fn write32(&self, offset: usize, value: u32) {
        self.mmio.write_reg::<u32>(offset, value);
    }
    fn write8(&self, offset: usize, value: u8) {
        self.mmio.write_reg::<u8>(offset, value);
    }
}

/// Main driver loop for the 8125: subscribe to the IRQ, bind it into the command
/// endpoint, arm the **V2** interrupt block, then dispatch TX requests + IRQ
/// wakes (acking via the V2 ISR). Never returns.
pub fn run_io_loop_v2(
    nic: Nic,
    command_endpoint: EndpointCap,
    ingress_endpoint: Option<EndpointCap>,
) -> ! {
    let view = DeviceCapView { cap: nic.pci.cap() };
    let irq = match IrqNotification::<IrqSyscallBackend>::subscribe(&view, None) {
        Ok(n) => n,
        Err(_) => syscall_lib::exit(4),
    };
    if irq.bind_to_endpoint(command_endpoint).is_err() {
        syscall_lib::exit(5);
    }

    // Arm the 32-bit V2 interrupt block (vs the classic 16-bit IMR the r8169
    // bring-up unmasked). `uses_v2` is true for all 8125/8126 parts.
    {
        let bridge = V2Bridge { mmio: &nic.mmio };
        interrupt::mask_all_v2(&bridge);
        interrupt::arm_v2(&bridge);
    }

    let net_server = match ingress_endpoint {
        Some(ep) => NetServer::new(command_endpoint).with_ingress_endpoint(ep),
        None => NetServer::new(command_endpoint),
    };

    // The service is already registered (READY printed) so the `net.nic` race
    // is won; only now do we wait for the PHY to finish auto-negotiating, and
    // log the outcome. Bounded poll (~5 s) of the MAC's read-only PHYstatus.
    let link_up = nic.wait_for_link(5_000);
    if link_up {
        syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "r8169: PHY link up\n");
    } else {
        syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "r8169: PHY link down\n");
    }

    // Publish link state to the kernel net stack. This is what bootstraps the
    // `RemoteNic` registration WITH OUR REAL MAC (read from MAC0) and marks the
    // interface usable — without it the kernel has MAC 00:00:00:00:00:00 and
    // never routes TX out to us (no ARP/ICMP ever reaches the driver). Mirrors
    // the e1000 driver. The ingress endpoint carries the event.
    let mac = nic.mac();
    {
        let id = ((mac[0] as u32) << 16) | ((mac[1] as u32) << 8) | mac[2] as u32;
        syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "r8125: station MAC oui=0x");
        syscall_lib::write_u64(syscall_lib::STDOUT_FILENO, id as u64);
        syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "\n");
    }
    let _ = net_server.publish_link_state(NetLinkEvent {
        up: link_up,
        mac,
        speed_mbps: 0,
    });

    // Re-assert ChipCmd RxEnb|TxEnb now that auto-negotiation has brought the
    // link up: the 8125 drops these bits when asserted (in `enable()`) while the
    // link is still down, leaving the RX/TX engines off. Re-enabling post-link
    // is what actually starts the datapath.
    if link_up {
        nic.chipcmd_enable();
    }

    let nic = core::cell::RefCell::new(nic);

    // Poll loop, not a block-on-IRQ loop. INTx delivery for a passed-through
    // (VFIO) NIC is unreliable: the physical card asserts its legacy line but it
    // may never reach the guest, so a block-on-`handle_next` loop would sleep
    // forever with the ARP/ICMP reply sitting undrained in the RX ring. Instead
    // we non-blocking-poll the command endpoint *and* drain the RX ring every
    // iteration, sleeping briefly only when there was nothing to do. This makes
    // the datapath independent of interrupt delivery (frames are picked up
    // within one poll interval) while still serving TX requests promptly and
    // acking any IRQ that does fire.
    let mut rx_total: u64 = 0;
    let tx_logged = core::cell::Cell::new(false);
    loop {
        let irq_bits = core::cell::Cell::new(0u64);
        let served = net_server
            .try_handle_next(
                |req| {
                    let mut dev = nic.borrow_mut();
                    let status = match send_frame(&mut dev, &req.frame) {
                        Ok(()) => {
                            if !tx_logged.get() {
                                tx_logged.set(true);
                                syscall_lib::write_str(
                                    syscall_lib::STDOUT_FILENO,
                                    "r8125: first TX frame sent, len=",
                                );
                                syscall_lib::write_u64(
                                    syscall_lib::STDOUT_FILENO,
                                    req.frame.len() as u64,
                                );
                                syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "\n");
                            }
                            NetDriverError::Ok
                        }
                        Err(e) => e,
                    };
                    NetReply { status }
                },
                |bits| {
                    irq_bits.set(bits);
                },
            )
            .unwrap_or(false);

        let drained = {
            let mut dev = nic.borrow_mut();
            {
                let bridge = V2Bridge { mmio: &dev.mmio };
                let _isr = interrupt::ack_v2(&bridge);
            }
            drain_rx(&mut dev, &net_server)
        };
        if drained > 0 {
            let first = rx_total == 0;
            rx_total += drained as u64;
            // Log only the first RX drain — a one-shot "the receive datapath is
            // alive" signal — rather than spamming a line per frame.
            if first {
                syscall_lib::write_str(
                    syscall_lib::STDOUT_FILENO,
                    "r8125: first RX frame(s) drained\n",
                );
            }
        }

        let bits = irq_bits.get();
        if bits != 0 {
            let _ = irq.ack(bits);
        }

        // Yield the CPU when idle (no TX request served and no RX drained) so the
        // poll loop does not spin a core. ~2 ms keeps ping RTT well under the 1 s
        // echo interval while staying responsive.
        if !served && drained == 0 {
            let _ = syscall_lib::nanosleep_for(0, 2_000_000);
        }
    }
}
