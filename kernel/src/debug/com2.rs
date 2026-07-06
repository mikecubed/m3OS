//! Polled COM2 (`0x2F8`) transport for the in-kernel GDB stub (Phase 111
//! Track C.3).
//!
//! Deliberately **not** COM1 (the live, IRQ-driven console) and **not** the
//! TCP stack: when the stub owns the CPU at a breakpoint the rest of the
//! kernel — including every interrupt path — is frozen, so the only transport
//! that works is synchronous register polling on a dedicated UART. QEMU routes
//! COM2 to a host TCP port via a second `-serial` argument (the first is
//! COM1's `stdio`); on bare metal it is a physical serial port.
//!
//! No allocation, no locks, no interrupts (IER=0): every function is safe to
//! call from the frozen all-stop stub loop, including from a panic or an NMI
//! context.

use x86_64::instructions::port::Port;

/// COM2 I/O port base.
const COM2_BASE: u16 = 0x2F8;
/// Data register (read RX / write TX). DLAB=1: divisor latch low byte.
const DATA: u16 = COM2_BASE;
/// Interrupt-enable register. DLAB=1: divisor latch high byte.
const IER: u16 = COM2_BASE + 1;
/// FIFO control register (write).
const FCR: u16 = COM2_BASE + 2;
/// Line control register (DLAB bit 7, word length bits 0-1).
const LCR: u16 = COM2_BASE + 3;
/// Modem control register (DTR/RTS).
const MCR: u16 = COM2_BASE + 4;
/// Line status register: bit 0 = RX data ready, bit 5 = THR empty.
const LSR: u16 = COM2_BASE + 5;

const LSR_RX_READY: u8 = 1 << 0;
const LSR_THR_EMPTY: u8 = 1 << 5;

#[inline]
fn outb(port: u16, v: u8) {
    // SAFETY: COM2 register I/O — the port range 0x2F8-0x2FF is owned by this
    // module (nothing else in the tree touches COM2).
    unsafe { Port::new(port).write(v) }
}

#[inline]
fn inb(port: u16) -> u8 {
    // SAFETY: as above — side-effect-contained UART register read.
    unsafe { Port::<u8>::new(port).read() }
}

/// Initialize COM2 at 115200 8N1 with interrupts **off** (the stub polls) and
/// FIFOs enabled. Idempotent; called once from `gdbstub::init`.
pub fn init() {
    outb(IER, 0x00); // no interrupts — polled transport
    outb(LCR, 0x80); // DLAB=1 to program the divisor
    outb(DATA, 0x01); // divisor low: 1 → 115200 baud
    outb(IER, 0x00); // divisor high
    outb(LCR, 0x03); // DLAB=0, 8 data bits, no parity, 1 stop bit
    outb(FCR, 0xC7); // enable + clear FIFOs, 14-byte RX trigger
    outb(MCR, 0x03); // assert DTR + RTS
}

/// Non-blocking read: one byte if the RX FIFO has data, else `None`.
#[inline]
pub fn try_read_byte() -> Option<u8> {
    if inb(LSR) & LSR_RX_READY != 0 {
        Some(inb(DATA))
    } else {
        None
    }
}

/// True if a byte is waiting in the RX FIFO (does not consume it). Used by the
/// async-break poll as a cheap per-tick guard before doing any real work.
#[inline]
pub fn rx_pending() -> bool {
    inb(LSR) & LSR_RX_READY != 0
}

/// Blocking write: spin until the transmit holding register is empty, then
/// send. The spin is HW-bounded (one UART character time at 115200 ≈ 87 µs).
#[inline]
pub fn write_byte(b: u8) {
    while inb(LSR) & LSR_THR_EMPTY == 0 {
        core::hint::spin_loop();
    }
    outb(DATA, b);
}

/// Write a full buffer.
pub fn write_all(bytes: &[u8]) {
    for &b in bytes {
        write_byte(b);
    }
}
