//! Phase 69d follow-up — `winsize-bang`.
//!
//! Tiny harness helper: forks a background child that sleeps a short
//! delay (so its calling shell has time to launch the next foreground
//! command, e.g. `htop`), then issues `TIOCSWINSZ` on stdin (inherited
//! from the calling shell, which is a TTY).  The kernel routes the
//! ioctl into a `SIGWINCH` to the TTY's foreground process group —
//! which is the redraw signal the `tui-app-smoke` htop reflow
//! assertion drives off.
//!
//! Why fork?  sh0 / ion (the smoke harness's login shell) has no `&`
//! job control, so "schedule a timer, then launch htop" is implemented
//! inside the helper itself: parent returns immediately, child does
//! the timed resize.  The shell waits on the parent, sees it exit, and
//! moves on to launching htop — meanwhile the child waits
//! `DELAY_SECONDS` and fires the ioctl.
//!
//! Why stdin instead of `/dev/tty`?  On m3OS, opening `/dev/tty` from
//! an arbitrary process requires `controlling_tty` to be set in the
//! process struct, and ion (the post-login shell) doesn't always
//! propagate that.  But stdin (fd 0) is always inherited from the
//! invoking shell — and the shell's stdin **is** the controlling TTY
//! by definition.  Issuing TIOCSWINSZ on fd 0 reaches the same kernel
//! TTY layer with no `controlling_tty` dependence.

#![no_std]
#![no_main]

use syscall_lib::{
    STDIN_FILENO, STDOUT_FILENO, TIOCSWINSZ, Winsize, exit, fork, ioctl, nanosleep, write_str,
    write_u64,
};

/// Geometry the helper applies on its single TIOCSWINSZ call.  Smaller
/// than the 24x80 default so htop has to wrap or truncate its header
/// on the redraw.
const ROWS: u16 = 20;
const COLS: u16 = 60;

/// How long the timer child sleeps before issuing the ioctl.  Long
/// enough that the calling shell has comfortably moved on to launching
/// htop AND htop has finished its initial `ncurses` setup and drawn
/// its first frame (the smoke gate waits on `Tasks:` before this).
/// 5 s gives ~3 s of headroom over the observed ~2 s htop initscr
/// path under TCG and prevents the race where `winsize-bang:fired`
/// outraces the first `Tasks:` print on a slow CI host.
const DELAY_SECONDS: u64 = 5;

// Phase 86f FIX 2: naked _start trampoline.  This binary ignores argv/envp.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor rbp, rbp",
        "call {f}",
        f = sym winsize_bang_main,
    );
}

fn winsize_bang_main() -> ! {
    let pid = fork();
    if pid < 0 {
        write_str(STDOUT_FILENO, "winsize-bang: fork failed\n");
        exit(1);
    }
    if pid > 0 {
        // Parent — return immediately so the shell prompt comes back.
        exit(0);
    }
    // Child — sleep then resize on inherited stdin.
    let _ = nanosleep(DELAY_SECONDS);

    let ws = Winsize {
        ws_row: ROWS,
        ws_col: COLS,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ret = ioctl(STDIN_FILENO, TIOCSWINSZ, &ws as *const Winsize as usize);
    if ret < 0 {
        write_str(STDOUT_FILENO, "winsize-bang: TIOCSWINSZ failed\n");
        exit(2);
    }
    // Sentinel printed AFTER the resize fires.  Carries the geometry
    // applied so the harness can prove TIOCSWINSZ → kernel TTY layer
    // → consumer-visible round-trip independently of what htop
    // chooses to redraw.  Format from the `COLS`/`ROWS` constants so
    // the sentinel cannot drift away from the ioctl payload (PR #177
    // fifth-pass review fix).
    write_str(STDOUT_FILENO, "winsize-bang:fired cols=");
    write_u64(STDOUT_FILENO, COLS as u64);
    write_str(STDOUT_FILENO, " rows=");
    write_u64(STDOUT_FILENO, ROWS as u64);
    write_str(STDOUT_FILENO, "\n");
    exit(0);
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "winsize-bang: PANIC\n");
    exit(101)
}
