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
use kernel_core::driver_ipc::net::NetDriverError;

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

    let nic = core::cell::RefCell::new(nic);
    let net_server = match ingress_endpoint {
        Some(ep) => NetServer::new(command_endpoint).with_ingress_endpoint(ep),
        None => NetServer::new(command_endpoint),
    };

    loop {
        let irq_bits = core::cell::Cell::new(0u64);
        let _ = net_server.handle_next(
            |req| {
                let mut dev = nic.borrow_mut();
                let status = match send_frame(&mut dev, &req.frame) {
                    Ok(()) => NetDriverError::Ok,
                    Err(e) => e,
                };
                NetReply { status }
            },
            |bits| {
                irq_bits.set(bits);
            },
        );

        let bits = irq_bits.get();
        if bits != 0 {
            let mut dev = nic.borrow_mut();
            // Ack via the V2 ISR (write-1-clear), then drain RX.
            {
                let bridge = V2Bridge { mmio: &dev.mmio };
                let _isr = interrupt::ack_v2(&bridge);
            }
            let _ = drain_rx(&mut dev, &net_server);
            drop(dev);
            let _ = irq.ack(bits);
        }
    }
}
