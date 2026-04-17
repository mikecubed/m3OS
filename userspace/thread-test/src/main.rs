#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};
use syscall_lib::{
    WNOHANG, exit, fork, gettimeofday, serial_print, syscall0, syscall1, syscall6, waitpid,
};

const SYS_CLONE: u64 = 56;
const SYS_EXIT: u64 = 60;
const SYS_GETPID: u64 = 39;
const SYS_GETTID: u64 = 186;
const SYS_FUTEX: u64 = 202;
const SYS_EXIT_GROUP: u64 = 231;

const CLONE_VM: u64 = 0x00000100;
const CLONE_FS: u64 = 0x00000200;
const CLONE_FILES: u64 = 0x00000400;
const CLONE_SIGHAND: u64 = 0x00000800;
const CLONE_THREAD: u64 = 0x00010000;
const CLONE_SETTLS: u64 = 0x00080000;
const CLONE_PARENT_SETTID: u64 = 0x00100000;
const CLONE_CHILD_CLEARTID: u64 = 0x00200000;

const FUTEX_WAIT: u64 = 0;
const FUTEX_WAKE: u64 = 1;
const FUTEX_PRIVATE_FLAG: u64 = 128;

const THREAD_STACK_SIZE: usize = 4096;

static SHARED_VALUE: AtomicU32 = AtomicU32::new(0);
static CHILD_TID: AtomicU32 = AtomicU32::new(0);
static CHILD_DONE: AtomicU32 = AtomicU32::new(0);
static CHILD_REPORTED_PID: AtomicU32 = AtomicU32::new(0);
static CHILD_REPORTED_TID: AtomicU32 = AtomicU32::new(0);
static EXIT_GROUP_SPINNER_STARTED: AtomicU32 = AtomicU32::new(0);

static MUTEX_WORD: AtomicU32 = AtomicU32::new(0);
static COUNTER: AtomicU32 = AtomicU32::new(0);

fn print_num(n: u64) {
    if n == 0 {
        serial_print("0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 0;
    let mut v = n;
    while v > 0 {
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        let ch = [buf[i]];
        if let Ok(s) = core::str::from_utf8(&ch) {
            serial_print(s);
        }
    }
}

fn futex_wait(addr: &AtomicU32, expected: u32) {
    unsafe {
        syscall6(
            SYS_FUTEX,
            addr as *const AtomicU32 as u64,
            FUTEX_WAIT | FUTEX_PRIVATE_FLAG,
            expected as u64,
            0,
            0,
            0,
        );
    }
}

fn futex_wake(addr: &AtomicU32, count: u32) {
    unsafe {
        syscall6(
            SYS_FUTEX,
            addr as *const AtomicU32 as u64,
            FUTEX_WAKE | FUTEX_PRIVATE_FLAG,
            count as u64,
            0,
            0,
            0,
        );
    }
}

fn mutex_lock(word: &AtomicU32) {
    loop {
        if word
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        futex_wait(word, 1);
    }
}

fn mutex_unlock(word: &AtomicU32) {
    word.store(0, Ordering::Release);
    futex_wake(word, 1);
}

extern "C" fn thread_fn_basic() -> ! {
    SHARED_VALUE.store(42, Ordering::Release);
    let my_pid = unsafe { syscall0(SYS_GETPID) } as u32;
    let my_tid = unsafe { syscall0(SYS_GETTID) } as u32;
    CHILD_REPORTED_PID.store(my_pid, Ordering::Release);
    CHILD_REPORTED_TID.store(my_tid, Ordering::Release);
    unsafe { syscall1(SYS_EXIT, 0) };
    exit(1)
}

extern "C" fn thread_fn_mutex() -> ! {
    for _ in 0..100 {
        mutex_lock(&MUTEX_WORD);
        let v = COUNTER.load(Ordering::Relaxed);
        COUNTER.store(v + 1, Ordering::Relaxed);
        mutex_unlock(&MUTEX_WORD);
    }
    unsafe { syscall1(SYS_EXIT, 0) };
    exit(1)
}

extern "C" fn thread_fn_exit_only() -> ! {
    CHILD_DONE.store(1, Ordering::Release);
    unsafe { syscall1(SYS_EXIT, 0) };
    exit(1)
}

extern "C" fn thread_fn_exit_group_spin() -> ! {
    EXIT_GROUP_SPINNER_STARTED.store(1, Ordering::Release);
    futex_wake(&EXIT_GROUP_SPINNER_STARTED, 1);
    loop {
        let _ = unsafe { syscall0(SYS_GETTID) };
        core::hint::spin_loop();
    }
}

static mut STACK1: [u8; THREAD_STACK_SIZE] = [0u8; THREAD_STACK_SIZE];
static mut STACK2: [u8; THREAD_STACK_SIZE] = [0u8; THREAD_STACK_SIZE];
static mut STACK3: [u8; THREAD_STACK_SIZE] = [0u8; THREAD_STACK_SIZE];
static mut STACK4: [u8; THREAD_STACK_SIZE] = [0u8; THREAD_STACK_SIZE];
static mut STACK5: [u8; THREAD_STACK_SIZE] = [0u8; THREAD_STACK_SIZE];

fn get_stack_top(stack: *mut [u8; THREAD_STACK_SIZE]) -> u64 {
    stack as u64 + THREAD_STACK_SIZE as u64
}

fn clone_thread(stack_top_addr: u64, entry: extern "C" fn() -> !, child_tid_ptr: *mut u32) -> u64 {
    let flags = CLONE_VM
        | CLONE_FS
        | CLONE_FILES
        | CLONE_SIGHAND
        | CLONE_THREAD
        | CLONE_SETTLS
        | CLONE_PARENT_SETTID
        | CLONE_CHILD_CLEARTID;

    let entry_addr = entry as usize as u64;
    let result: u64;
    unsafe {
        core::arch::asm!(
            // Align child stack to 16 bytes and push the entry fn pointer
            "and rsi, -16",
            "sub rsi, 8",
            "mov [rsi], r9",

            // syscall: clone(flags, child_stack, parent_tidptr, child_tidptr, tls)
            "mov rax, {sys_clone}",
            "syscall",

            // Parent gets child_tid (> 0), child gets 0
            "test rax, rax",
            "jnz 2f",

            // Child path: pop fn pointer and call it
            "pop rdi",
            "call rdi",
            "mov rax, {sys_exit}",
            "xor edi, edi",
            "syscall",

            "2:",

            sys_clone = const SYS_CLONE,
            sys_exit = const SYS_EXIT,
            in("rdi") flags,
            in("rsi") stack_top_addr,
            in("rdx") child_tid_ptr as u64,
            in("r10") child_tid_ptr as u64,
            in("r8") 0u64,
            in("r9") entry_addr,
            lateout("rax") result,
            lateout("rcx") _,
            lateout("r11") _,
            clobber_abi("C"),
        );
    }
    result
}

fn wait_for_thread(child_tid_ptr: &AtomicU32) {
    loop {
        let val = child_tid_ptr.load(Ordering::Acquire);
        if val == 0 {
            return;
        }
        futex_wait(child_tid_ptr, val);
    }
}

fn test_basic_thread() -> bool {
    serial_print("thread-test: test 1 -- basic create/join... ");

    SHARED_VALUE.store(0, Ordering::Release);
    CHILD_TID.store(0, Ordering::Release);
    CHILD_REPORTED_PID.store(0, Ordering::Release);
    CHILD_REPORTED_TID.store(0, Ordering::Release);

    let parent_pid = unsafe { syscall0(SYS_GETPID) };
    let parent_tid = unsafe { syscall0(SYS_GETTID) };

    let child_tid_addr = &CHILD_TID as *const AtomicU32 as *mut u32;
    let child_tid = clone_thread(
        get_stack_top(&raw mut STACK1),
        thread_fn_basic,
        child_tid_addr,
    );

    if (child_tid as i64) < 0 || child_tid == 0 {
        serial_print("FAIL (clone returned ");
        let signed = child_tid as i64;
        if signed < 0 {
            serial_print("-");
            print_num((-signed) as u64);
        } else {
            print_num(child_tid);
        }
        serial_print(")\n");
        return false;
    }

    if child_tid == parent_tid {
        serial_print("FAIL (same tid)\n");
        return false;
    }

    wait_for_thread(&CHILD_TID);

    let val = SHARED_VALUE.load(Ordering::Acquire);
    if val != 42 {
        serial_print("FAIL (shared_value=");
        print_num(val as u64);
        serial_print(")\n");
        return false;
    }

    // Verify child reported the same TGID (getpid) but different TID (gettid).
    let child_pid = CHILD_REPORTED_PID.load(Ordering::Acquire) as u64;
    let child_tid_reported = CHILD_REPORTED_TID.load(Ordering::Acquire) as u64;

    if child_pid != parent_pid {
        serial_print("FAIL (child getpid()=");
        print_num(child_pid);
        serial_print(" != parent getpid()=");
        print_num(parent_pid);
        serial_print(")\n");
        return false;
    }

    if child_tid_reported == parent_tid {
        serial_print("FAIL (child gettid() == parent gettid())\n");
        return false;
    }

    serial_print("PASS (parent_pid=");
    print_num(parent_pid);
    serial_print(", parent_tid=");
    print_num(parent_tid);
    serial_print(", child_tid=");
    print_num(child_tid);
    serial_print(", child_getpid=");
    print_num(child_pid);
    serial_print(", child_gettid=");
    print_num(child_tid_reported);
    serial_print(")\n");
    true
}

fn test_futex_mutex() -> bool {
    serial_print("thread-test: test 2 -- futex mutex stress... ");

    MUTEX_WORD.store(0, Ordering::Release);
    COUNTER.store(0, Ordering::Release);

    static CHILD_TID2A: AtomicU32 = AtomicU32::new(0);
    static CHILD_TID2B: AtomicU32 = AtomicU32::new(0);

    CHILD_TID2A.store(0, Ordering::Release);
    CHILD_TID2B.store(0, Ordering::Release);

    let t1 = clone_thread(
        get_stack_top(&raw mut STACK2),
        thread_fn_mutex,
        &CHILD_TID2A as *const AtomicU32 as *mut u32,
    );
    let t2 = clone_thread(
        get_stack_top(&raw mut STACK3),
        thread_fn_mutex,
        &CHILD_TID2B as *const AtomicU32 as *mut u32,
    );

    if t1 == u64::MAX || t1 == 0 || t2 == u64::MAX || t2 == 0 {
        serial_print("FAIL (clone failed)\n");
        return false;
    }

    for _ in 0..100 {
        mutex_lock(&MUTEX_WORD);
        let v = COUNTER.load(Ordering::Relaxed);
        COUNTER.store(v + 1, Ordering::Relaxed);
        mutex_unlock(&MUTEX_WORD);
    }

    wait_for_thread(&CHILD_TID2A);
    wait_for_thread(&CHILD_TID2B);

    let final_count = COUNTER.load(Ordering::Acquire);
    if final_count != 300 {
        serial_print("FAIL (counter=");
        print_num(final_count as u64);
        serial_print(", expected 300)\n");
        return false;
    }

    serial_print("PASS (counter=300)\n");
    true
}

fn test_thread_exit() -> bool {
    serial_print("thread-test: test 3 -- thread exit... ");

    CHILD_DONE.store(0, Ordering::Release);

    static CHILD_TID3: AtomicU32 = AtomicU32::new(0);
    CHILD_TID3.store(0, Ordering::Release);

    let t = clone_thread(
        get_stack_top(&raw mut STACK4),
        thread_fn_exit_only,
        &CHILD_TID3 as *const AtomicU32 as *mut u32,
    );

    if t == u64::MAX || t == 0 {
        serial_print("FAIL (clone failed)\n");
        return false;
    }

    wait_for_thread(&CHILD_TID3);

    let done = CHILD_DONE.load(Ordering::Acquire);
    if done != 1 {
        serial_print("FAIL (child didn't set flag)\n");
        return false;
    }

    serial_print("PASS\n");
    true
}

fn now_micros() -> i128 {
    let (sec, usec) = gettimeofday();
    if sec < 0 {
        -1
    } else {
        (sec as i128) * 1_000_000 + (usec as i128)
    }
}

/// Returns true when `deadline_us` has elapsed or the clock is unavailable
/// (so loops can't hang forever if `gettimeofday` keeps failing).
fn deadline_reached(deadline_us: i128) -> bool {
    let now = now_micros();
    now < 0 || now >= deadline_us
}

fn arm_exit_group_spinner() -> bool {
    static CHILD_TID4: AtomicU32 = AtomicU32::new(0);
    CHILD_TID4.store(0, Ordering::Release);
    EXIT_GROUP_SPINNER_STARTED.store(0, Ordering::Release);

    let t = clone_thread(
        get_stack_top(&raw mut STACK5),
        thread_fn_exit_group_spin,
        &CHILD_TID4 as *const AtomicU32 as *mut u32,
    );

    if t == u64::MAX || t == 0 {
        return false;
    }

    let start = now_micros();
    if start < 0 {
        return false;
    }
    let deadline_us = start + 5_000_000;

    loop {
        if EXIT_GROUP_SPINNER_STARTED.load(Ordering::Acquire) != 0 {
            return true;
        }
        let _ = unsafe { syscall0(SYS_GETTID) };
        if deadline_reached(deadline_us) {
            break;
        }
    }

    false
}

fn test_exit_group_teardown() -> bool {
    serial_print("thread-test: test 4 -- exit_group live sibling... ");

    let pid = fork();
    if pid < 0 {
        serial_print("FAIL (fork failed)\n");
        return false;
    }

    if pid == 0 {
        if !arm_exit_group_spinner() {
            exit(2);
        }
        unsafe { syscall1(SYS_EXIT_GROUP, 0) };
        exit(3)
    }

    let child_pid = pid as i32;
    let start = now_micros();
    if start < 0 {
        serial_print("FAIL (gettimeofday failed)\n");
        return false;
    }
    let deadline_us = start + 5_000_000;
    let mut status = 0i32;

    loop {
        let waited = waitpid(child_pid, &mut status, WNOHANG);
        if waited == pid {
            let exit_code = (status >> 8) & 0xff;
            match exit_code {
                0 => {
                    serial_print("PASS\n");
                    return true;
                }
                2 => {
                    serial_print("FAIL (spinner start timeout)\n");
                    return false;
                }
                3 => {
                    serial_print("FAIL (exit_group returned)\n");
                    return false;
                }
                code => {
                    serial_print("FAIL (child exit=");
                    print_num(code as u64);
                    serial_print(")\n");
                    return false;
                }
            }
        }
        if waited < 0 {
            serial_print("FAIL (waitpid failed)\n");
            return false;
        }
        if deadline_reached(deadline_us) {
            serial_print("FAIL (waitpid timeout)\n");
            return false;
        }
        let _ = unsafe { syscall0(SYS_GETTID) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    serial_print("thread-test: starting threading primitive tests\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    if test_basic_thread() {
        passed += 1;
    } else {
        failed += 1;
    }

    if test_futex_mutex() {
        passed += 1;
    } else {
        failed += 1;
    }

    if test_thread_exit() {
        passed += 1;
    } else {
        failed += 1;
    }

    if test_exit_group_teardown() {
        passed += 1;
    } else {
        failed += 1;
    }

    serial_print("thread-test: ");
    print_num(passed as u64);
    serial_print(" passed, ");
    print_num(failed as u64);
    serial_print(" failed\n");

    if failed == 0 {
        serial_print("thread-test: ALL TESTS PASSED\n");
    }

    exit(if failed == 0 { 0 } else { 1 })
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    serial_print("thread-test: PANIC\n");
    exit(99)
}
