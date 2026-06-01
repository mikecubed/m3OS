//! Phase 80b — Intel HDA out-of-process ring-3 driver entry point.
//!
//! Run-time flow: write the boot marker, discover an HDA controller by PCI
//! class (0x04/0x03/0x00, vendor-agnostic), claim it, map BAR0, bring up the
//! controller (reset → STATESTS codec-ready → CORB/RIRB RUN-enable), create an
//! IPC endpoint, register it as `"audio.hw"`, emit `HDA_SMOKE:server:READY`,
//! and enter the `audio.hw` server loop serving the `driver_ipc::audio`
//! protocol. The codec output path + stream descriptor are configured lazily
//! on the first `OpenStream`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(not(test))]
use alloc::vec;
#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use driver_runtime::audio_pcm::{self, PcmReceiver};
#[cfg(not(test))]
use driver_runtime::ipc::audio::{decode_request_bulk, reply_response};
#[cfg(not(test))]
use driver_runtime::ipc::{EndpointCap, RecvResult, SyscallBackend};
#[cfg(not(test))]
use driver_runtime::{
    DeviceCapHandle, DeviceHandle, IrqNotification, Mmio, SyscallBackend as IrqSyscallBackend,
};
#[cfg(not(test))]
use hda_driver::codec::OutputPath;
#[cfg(not(test))]
use hda_driver::controller::HdaController;
#[cfg(not(test))]
use hda_driver::stream::OutputStream;
#[cfg(not(test))]
use hda_driver::{BOOT_LOG_MARKER, HDA_BAR0_LEN, SERVER_READY_SENTINEL, SERVICE_NAME};
#[cfg(not(test))]
use kernel_core::driver_ipc::audio::{
    AUDIO_REQUEST_MAX_SIZE, AudioDriverError, AudioRequest, AudioResponse, caps_v1,
};
#[cfg(not(test))]
use syscall_lib::STDOUT_FILENO;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "hda_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "hda_driver: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Single output stream tag (`audio_server` is single-stream for 1.0).
#[cfg(not(test))]
const STREAM_TAG: u8 = 1;
/// Facing stream id returned to `audio_server` (it is single-stream).
#[cfg(not(test))]
const FACING_STREAM_ID: u32 = 1;

/// Wraps a borrowed [`DeviceHandle`] as a [`DeviceCapHandle`] for
/// [`IrqNotification::subscribe`].
#[cfg(not(test))]
struct DeviceCapView<'a> {
    inner: &'a DeviceHandle,
}

#[cfg(not(test))]
impl DeviceCapHandle for DeviceCapView<'_> {
    fn cap_handle(&self) -> u32 {
        self.inner.cap()
    }
}

/// Discover an HDA controller by PCI class (vendor-agnostic 0x04/0x03/0x00).
#[cfg(not(test))]
fn find_hda() -> Option<driver_runtime::DeviceCapKey> {
    let candidates = driver_runtime::enumerate_pci_class(
        kernel_core::hda::ids::HDA_CLASS,
        kernel_core::hda::ids::HDA_SUBCLASS,
        kernel_core::hda::ids::HDA_PROG_IF,
    )
    .ok()?;
    candidates.into_iter().next()
}

#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, BOOT_LOG_MARKER);

    let key = match find_hda() {
        Some(k) => k,
        None => {
            // No HDA controller (e.g. QEMU with only -device AC97) — exit
            // cleanly so the ac97 driver serves audio.hw.
            syscall_lib::write_str(STDOUT_FILENO, "hda_driver: no HDA controller present\n");
            return 0;
        }
    };

    let device = match DeviceHandle::claim(key) {
        Ok(d) => d,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "hda_driver: device claim failed\n");
            return 1;
        }
    };

    let bar0 = match Mmio::<u8>::map(&device, 0, HDA_BAR0_LEN) {
        Ok(m) => m,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "hda_driver: BAR0 map failed\n");
            return 2;
        }
    };

    let mut controller = match HdaController::bring_up(&device, bar0) {
        Ok(c) => c,
        Err(e) => {
            syscall_lib::write_str(STDOUT_FILENO, "hda_driver: controller bring-up failed: ");
            syscall_lib::write_str(STDOUT_FILENO, e);
            syscall_lib::write_str(STDOUT_FILENO, "\n");
            return 3;
        }
    };
    syscall_lib::write_str(STDOUT_FILENO, "hda_driver: controller up, codecs ready\n");

    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "hda_driver: endpoint create failed\n");
        return 4;
    }
    let ep_u32 = match u32::try_from(ep) {
        Ok(id) => id,
        Err(_) => return 6,
    };
    if syscall_lib::ipc_register_service(ep_u32, SERVICE_NAME) == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "hda_driver: service register failed\n");
        return 5;
    }

    // C.3: subscribe to the HDA IRQ and bind it into the endpoint recv loop so
    // a single loop services both client requests and stream-completion (BCIS)
    // interrupts. Falls back to SDnLPIB polling if subscription is unavailable.
    let endpoint = EndpointCap::new(ep_u32);
    let irq: Option<IrqNotification<IrqSyscallBackend>> = {
        let view = DeviceCapView { inner: &device };
        match IrqNotification::<IrqSyscallBackend>::subscribe(&view, None) {
            Ok(n) => {
                let _ = n.bind_to_endpoint(endpoint);
                // Arm INTCTL only after a handler is bound — arming without a
                // subscriber would leave the controller asserting an unhandled
                // PCI IRQ on every buffer completion.
                controller.arm_interrupts();
                syscall_lib::write_str(STDOUT_FILENO, "hda_driver: IRQ armed (subscribed)\n");
                Some(n)
            }
            Err(_) => {
                syscall_lib::write_str(
                    STDOUT_FILENO,
                    "hda_driver: IRQ subscribe unavailable — SDnLPIB polling fallback\n",
                );
                None
            }
        }
    };

    syscall_lib::write_str(STDOUT_FILENO, SERVER_READY_SENTINEL);

    server_loop(&device, &mut controller, endpoint, irq.as_ref())
}

/// `audio.hw` server loop: serves the `driver_ipc::audio` protocol, copying
/// each `SubmitFrames` shared-ring window into the output stream's cyclic DMA
/// buffer.
#[cfg(not(test))]
fn server_loop(
    device: &DeviceHandle,
    controller: &mut HdaController,
    endpoint: EndpointCap,
    irq: Option<&IrqNotification<IrqSyscallBackend>>,
) -> i32 {
    let mut backend = SyscallBackend::new();
    let mut receiver = PcmReceiver::new();
    let mut scratch = vec![0u8; audio_pcm::PCM_RING_BYTES];
    let mut stream: Option<OutputStream> = None;
    let mut _path: Option<OutputPath> = None;
    let mut logged_irq = false;

    loop {
        let frame = match backend.recv_with_capacity(endpoint, AUDIO_REQUEST_MAX_SIZE) {
            Ok(RecvResult::Message(f)) => f,
            Ok(RecvResult::Notification(bits)) => {
                // Stream-completion (BCIS) interrupt: clear it so it does not
                // re-assert, then ack the notification.
                if controller.handle_irq() && !logged_irq {
                    syscall_lib::write_str(
                        STDOUT_FILENO,
                        "hda_driver: stream IRQ (BCIS cleared)\n",
                    );
                    logged_irq = true;
                }
                if let Some(irq) = irq {
                    let _ = irq.ack(bits);
                }
                continue;
            }
            Err(_) => continue,
        };

        let rsp = match decode_request_bulk(&frame.bulk) {
            Ok(AudioRequest::QueryCaps) => AudioResponse::Caps(caps_v1()),
            Ok(AudioRequest::OpenStream { .. }) => {
                match hda_driver::stream::open_output(device, controller, STREAM_TAG) {
                    Ok((s, p)) => {
                        stream = Some(s);
                        _path = Some(p);
                        AudioResponse::StreamOpened(FACING_STREAM_ID)
                    }
                    Err(_) => AudioResponse::Err(AudioDriverError::Internal),
                }
            }
            Ok(AudioRequest::SubmitFrames {
                grant_handle,
                offset,
                len,
                ..
            }) => match stream.as_mut() {
                None => AudioResponse::Err(AudioDriverError::InvalidArgument),
                Some(s) => {
                    let n = (len as usize).min(scratch.len());
                    match receiver.recv_and_copy(grant_handle, offset, len, &mut scratch[..n]) {
                        Ok(copied) => {
                            if s.submit(&controller.mmio, &scratch[..copied]) {
                                let fc = s.poll_consumed(&controller.mmio);
                                // Proactively clear any pending BCIS during the
                                // poll path. SDnLPIB polling is the authoritative
                                // completion path (deferred DPB); this de-asserts
                                // the level-triggered INTx line even when the
                                // bound IRQ notification is not delivered, so the
                                // armed interrupt can never storm.
                                let _ = controller.handle_irq();
                                AudioResponse::Ack {
                                    frames_consumed: fc,
                                }
                            } else {
                                AudioResponse::WouldBlock
                            }
                        }
                        Err(_) => AudioResponse::Err(AudioDriverError::InvalidArgument),
                    }
                }
            },
            Ok(AudioRequest::Drain { .. }) => AudioResponse::Ok,
            Ok(AudioRequest::CloseStream { .. }) => {
                if let Some(s) = stream.take() {
                    s.stop(&controller.mmio);
                }
                _path = None;
                receiver.release();
                AudioResponse::Ok
            }
            Err(_) => AudioResponse::Err(AudioDriverError::InvalidArgument),
        };

        let _ = reply_response(&mut backend, &rsp);
    }
}
