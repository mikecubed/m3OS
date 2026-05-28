#![no_std]
#![no_main]

use core::ptr;

use syscall_lib::{
    O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, STDERR_FILENO, STDOUT_FILENO, Stat, close, dup2, execve,
    exit, fork, geteuid, getpid, open, read, stat, unlink, waitpid, write, write_str,
};

const TCC_PATH: &[u8] = b"/usr/bin/tcc\0";
const HELLO_SOURCE_PATH: &[u8] = b"/usr/src/hello.c\0";
const HELLO_BIN_PATH: &[u8] = b"/tmp/h\0";
const SKIP_TCC_MARKER: &[u8] = b"/etc/m3os-skip-tcc-compile\0";
const PASSWD_PATH: &[u8] = b"/etc/passwd\0";
const UDP_SMOKE_PATH: &[u8] = b"/root/udp-smoke\0";
const PAGE_GRANT_TEST_PATH: &[u8] = b"/bin/page-grant-test\0";
const PAGE_GRANT_TEST_ARGV0: &[u8] = b"page-grant-test\0";
const PAGE_GRANT_PASS_NEEDLE: &[u8] = b"PAGE_GRANT_SMOKE:roundtrip:ok";
const WX_VIOLATION_PATH: &[u8] = b"/bin/wx-violation\0";
const WX_VIOLATION_ARGV0: &[u8] = b"wx-violation\0";
const WX_VIOLATION_PASS_NEEDLE: &[u8] = b"WX_VIOLATION:smoke:ok";
// Phase 77 Track C — multi-threaded __thread / PT_TLS smoke test. A
// full-musl static binary; SKIP if the musl toolchain was absent at build.
const TLS_SMOKE_PATH: &[u8] = b"/bin/tls-smoke\0";
const TLS_SMOKE_ARGV0: &[u8] = b"tls-smoke\0";
const TLS_SMOKE_PASS_NEEDLE: &[u8] = b"TLS_SMOKE:PASS";
// Phase 77 Track D.1 — DNS resolution via the prebuilt musl resolver +
// staged /etc/resolv.conf. dns-smoke emits DNS_SMOKE:PASS when a name
// resolves or DNS_SMOKE:SKIP when no outbound DNS is reachable (exit 0
// either way); the gate accepts the common `DNS_SMOKE:` prefix.
const DNS_SMOKE_PATH: &[u8] = b"/bin/dns-smoke\0";
const DNS_SMOKE_ARGV0: &[u8] = b"dns-smoke\0";
const DNS_SMOKE_NEEDLE: &[u8] = b"DNS_SMOKE:";
// Phase 76 — `dynlink_smoke` is a musl-built dynamic ELF carrying
// `PT_INTERP = /lib/ld-musl-x86_64.so.1` and zero `DT_NEEDED`
// entries. Running it exercises the kernel `PT_INTERP` branch +
// the ld.so transfer-only stub end to end. The sentinel
// (`DYNLINK_SMOKE:PASS`) is written via inline-asm `syscall` so the
// binary touches no libc symbols and 76b's full bring-up is not
// required for the gate to pass.
const DYNLINK_SMOKE_PATH: &[u8] = b"/bin/dynlink_smoke\0";
const DYNLINK_SMOKE_ARGV0: &[u8] = b"dynlink_smoke\0";
const DYNLINK_SMOKE_PASS_NEEDLE: &[u8] = b"DYNLINK_SMOKE:PASS";
// Phase 76b — `dynlink_hello` is a musl-built dynamic ELF carrying
// `DT_NEEDED = libhello.so` and `PT_INTERP = /lib/ld-musl-x86_64.so.1`.
// The bring-up linker resolves `hello_str` via `DT_HASH` + `DT_JMPREL`
// (R_X86_64_JUMP_SLOT) and the binary prints the sentinel to stdout.
const DYNLINK_HELLO_PATH: &[u8] = b"/bin/dynlink_hello\0";
const DYNLINK_HELLO_ARGV0: &[u8] = b"dynlink_hello\0";
const DYNLINK_HELLO_PASS_NEEDLE: &[u8] = b"HELLO_FROM_SHARED_LIB:OK";
// Phase 76b F1.4 negative gates.
const DYNLINK_MISSING_PATH: &[u8] = b"/bin/dynlink_missing\0";
const DYNLINK_MISSING_ARGV0: &[u8] = b"dynlink_missing\0";
const DYNLINK_CYCLE_PATH: &[u8] = b"/bin/dynlink_cycle\0";
const DYNLINK_CYCLE_ARGV0: &[u8] = b"dynlink_cycle\0";
// Phase 76c — `dlopen_test` exercises dlopen / dlsym / dlclose /
// dlerror plus DT_FINI_ARRAY destructors. The smoke gate asserts
// the serial order FINI_PENDING → LIBHELLO_FINI:RAN → PASS so a
// missing destructor invocation between the bracket sentinels is a
// hard FAIL.
const DLOPEN_TEST_PATH: &[u8] = b"/bin/dlopen_test\0";
const DLOPEN_TEST_ARGV0: &[u8] = b"dlopen_test\0";
const DLOPEN_TEST_PASS_NEEDLE: &[u8] = b"DLOPEN_TEST:PASS";
const DLOPEN_TEST_PENDING_NEEDLE: &[u8] = b"DLOPEN_TEST:FINI_PENDING";
const DLOPEN_TEST_FINI_RAN_NEEDLE: &[u8] = b"LIBHELLO_FINI:RAN";
// Phase 76d.F — `dynlink_hello_gnu` exercises Phase 76d.D1's
// DT_GNU_HASH backend, B4's PLT lazy resolve trampoline, and F.4's
// W^X invariant. Two-phase gate:
//   * default-env run: BIND_NOW:0 (lazy) + HELLO_FROM_GNU_LIB:OK +
//     WX_CHECK:OK
//   * LD_BIND_NOW=1 run: BIND_NOW:1 (eager) + HELLO_FROM_GNU_LIB:OK +
//     WX_CHECK:OK (F.3 LD_BIND_NOW regression).
const DYNLINK_HELLO_GNU_PATH: &[u8] = b"/bin/dynlink_hello_gnu\0";
const DYNLINK_HELLO_GNU_ARGV0: &[u8] = b"dynlink_hello_gnu\0";
const DYNLINK_HELLO_GNU_PASS_NEEDLE: &[u8] = b"HELLO_FROM_GNU_LIB:OK";
const DYNLINK_HELLO_GNU_LAZY_NEEDLE: &[u8] = b"BIND_NOW:0";
const DYNLINK_HELLO_GNU_EAGER_NEEDLE: &[u8] = b"BIND_NOW:1";
const DYNLINK_HELLO_GNU_WX_NEEDLE: &[u8] = b"WX_CHECK:OK";
const LD_BIND_NOW_ENV: &[u8] = b"LD_BIND_NOW=1\0";
// Phase 76d.G — `dynlink_hello_versioned` exercises Phase 76d.D2.2
// version-aware lookup. The consumer's `DT_VERNEED` requires
// `LIBHELLO_1.0` against `libhello_versioned.so`; the lib defines
// that exact version via its `--version-script`. Success sentinel:
// the lib's exported `hello_str` returns the bytes below.
const DYNLINK_HELLO_VERSIONED_PATH: &[u8] = b"/bin/dynlink_hello_versioned\0";
const DYNLINK_HELLO_VERSIONED_ARGV0: &[u8] = b"dynlink_hello_versioned\0";
const DYNLINK_HELLO_VERSIONED_PASS_NEEDLE: &[u8] = b"HELLO_FROM_VERSIONED_LIB:OK";
// Phase 76d.G.3 — `dynlink_hello_versioned_mismatch` requests
// `LIBHELLO_1.0` against a lib (v2) that only provides
// `LIBHELLO_2.0`. Default mode → D2.2 warns + falls back to
// unversioned. Strict mode (`LD_BIND_NOW=1`) → D2.3 errors + exits.
const DYNLINK_HELLO_VERSIONED_MISMATCH_PATH: &[u8] = b"/bin/dynlink_hello_versioned_mismatch\0";
const DYNLINK_HELLO_VERSIONED_MISMATCH_ARGV0: &[u8] = b"dynlink_hello_versioned_mismatch\0";
const DYNLINK_HELLO_VERSIONED_MISMATCH_FALLBACK_NEEDLE: &[u8] = b"HELLO_FROM_V2_FALLBACK:OK";
const CAPTURE_FILE_PATH: &[u8] = b"/tmp/smoke-runner.capture\0";
const LOGGER_PATH: &[u8] = b"/bin/logger\0";
const SYSTEM_LOG_PATH: &[u8] = b"/var/log/messages\0";

const TCC_ARGV0: &[u8] = b"tcc\0";
const TCC_VERSION_ARG: &[u8] = b"--version\0";
const TCC_STATIC_ARG: &[u8] = b"-static\0";
const TCC_OUTPUT_ARG: &[u8] = b"-o\0";
const LOGGER_ARGV0: &[u8] = b"logger\0";

const TCC_VERSION_NEEDLE: &[u8] = b"tcc version";
const UDP_PASS_NEEDLE: &[u8] = b"udp-smoke: PASS";

const READ_BUF_LEN: usize = 4096;
const FILE_SCAN_BUF_LEN: usize = 1152;

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "SMOKE:BEGIN\n");

    begin("auth");
    if geteuid() != 0 {
        return fail("auth", "expected root shell session", 1);
    }
    pass("auth");

    let mut command_output = [0u8; READ_BUF_LEN];

    begin("tcc-version");
    let tcc_version_argv = [TCC_ARGV0.as_ptr(), TCC_VERSION_ARG.as_ptr(), ptr::null()];
    if let Err(code) = run_command_expect_output(
        "tcc-version",
        TCC_PATH,
        &tcc_version_argv,
        TCC_VERSION_NEEDLE,
        &mut command_output,
    ) {
        return code;
    }
    pass("tcc-version");

    // CI runs TCC + hello under pure TCG, which blows the per-step budget
    // (Phase 54 added IPC hops on every file op). The host drops a marker
    // file at /etc/m3os-skip-tcc-compile when running in a budget-constrained
    // environment; we then emit SKIP and the host treats that as equivalent
    // to PASS.
    let skip_tcc = marker_present(SKIP_TCC_MARKER);

    if skip_tcc {
        skip("tcc-compile");
        skip("hello");
    } else {
        begin("tcc-compile");
        let tcc_compile_argv = [
            TCC_ARGV0.as_ptr(),
            TCC_STATIC_ARG.as_ptr(),
            HELLO_SOURCE_PATH.as_ptr(),
            TCC_OUTPUT_ARG.as_ptr(),
            HELLO_BIN_PATH.as_ptr(),
            ptr::null(),
        ];
        if let Err(code) = run_command_expect_success(
            "tcc-compile",
            TCC_PATH,
            &tcc_compile_argv,
            &mut command_output,
        ) {
            return code;
        }
        pass("tcc-compile");

        begin("hello");
        if let Err(code) = verify_compiled_hello() {
            return code;
        }
        pass("hello");
    }

    begin("storage");
    if let Err(code) = verify_required_storage_files() {
        return code;
    }
    pass("storage");

    begin("net");
    let udp_smoke_argv = [UDP_SMOKE_PATH.as_ptr(), ptr::null()];
    if let Err(code) = run_command_expect_output(
        "net",
        UDP_SMOKE_PATH,
        &udp_smoke_argv,
        UDP_PASS_NEEDLE,
        &mut command_output,
    ) {
        return code;
    }
    pass("net");

    begin("log");
    if let Err(code) = inject_and_verify_log_marker(&mut command_output) {
        return code;
    }
    pass("log");

    // Phase 74 Track B.3 — page-grant round-trip regression. Validates
    // that `sys_page_grant_send` + `sys_page_grant_recv` actually move
    // 1024 pages without copying any bytes and that the consume side is
    // single-shot.
    begin("page-grant");
    let page_grant_argv = [PAGE_GRANT_TEST_ARGV0.as_ptr(), ptr::null()];
    if let Err(code) = run_command_expect_output(
        "page-grant",
        PAGE_GRANT_TEST_PATH,
        &page_grant_argv,
        PAGE_GRANT_PASS_NEEDLE,
        &mut command_output,
    ) {
        return code;
    }
    pass("page-grant");

    // Phase 75 Track G.1 — W^X enforcement regression. Validates the
    // new `sys_mprotect` guard rejects `PROT_WRITE | PROT_EXEC`
    // (EINVAL) and that the supported JIT pattern
    // (`PROT_READ | PROT_EXEC`) still succeeds.
    begin("wx-violation");
    let wx_argv = [WX_VIOLATION_ARGV0.as_ptr(), ptr::null()];
    if let Err(code) = run_command_expect_output(
        "wx-violation",
        WX_VIOLATION_PATH,
        &wx_argv,
        WX_VIOLATION_PASS_NEEDLE,
        &mut command_output,
    ) {
        return code;
    }
    pass("wx-violation");

    // Phase 77 Track C — multi-threaded __thread / PT_TLS test. Proves each
    // pthread sees its own copy of a `__thread` variable (the PT_TLS template
    // the ELF loader now recognises is copied per-thread by musl). SKIP if the
    // musl toolchain was missing at build (zero-byte placeholder).
    {
        let mut probe = Stat::zeroed();
        if stat(TLS_SMOKE_PATH, &mut probe) < 0 || probe.st_size == 0 {
            skip("tls-smoke");
        } else {
            begin("tls-smoke");
            let tls_argv = [TLS_SMOKE_ARGV0.as_ptr(), ptr::null()];
            if let Err(code) = run_command_expect_output(
                "tls-smoke",
                TLS_SMOKE_PATH,
                &tls_argv,
                TLS_SMOKE_PASS_NEEDLE,
                &mut command_output,
            ) {
                return code;
            }
            pass("tls-smoke");
        }
    }

    // Phase 77 Track D.1 — DNS resolution via the prebuilt musl resolver.
    // dns-smoke exits 0 with DNS_SMOKE:PASS (name resolved) or DNS_SMOKE:SKIP
    // (no outbound DNS) — both satisfy the gate, which only asserts the
    // resolver path ran and emitted a verdict. SKIP if the binary is absent
    // (musl toolchain missing at build).
    {
        let mut probe = Stat::zeroed();
        if stat(DNS_SMOKE_PATH, &mut probe) < 0 || probe.st_size == 0 {
            skip("dns-smoke");
        } else {
            begin("dns-smoke");
            let dns_argv = [DNS_SMOKE_ARGV0.as_ptr(), ptr::null()];
            if let Err(code) = run_command_expect_output(
                "dns-smoke",
                DNS_SMOKE_PATH,
                &dns_argv,
                DNS_SMOKE_NEEDLE,
                &mut command_output,
            ) {
                return code;
            }
            pass("dns-smoke");
        }
    }

    // Phase 76 — exercise the kernel PT_INTERP branch + ld.so
    // transfer-only stub. The dynlink_smoke binary writes the
    // sentinel below to stderr (fd 2) via inline-asm syscalls so it
    // has zero DT_NEEDED entries; the linker's only job is to walk
    // the auxv for AT_ENTRY and jmp. The smoke gate is the byte-exact
    // proof that kernel → ld.so → main hand-off works.
    //
    // The dynlink_smoke binary's build can SKIP at xtask time if
    // musl-gcc and host gcc are both missing (the staging step
    // detects a zero-byte placeholder and emits SKIP). Detect that
    // here so a missing C toolchain does not break the gate.
    {
        let mut probe = Stat::zeroed();
        if stat(DYNLINK_SMOKE_PATH, &mut probe) < 0 || probe.st_size == 0 {
            skip("dynlink-smoke");
        } else {
            begin("dynlink-smoke");
            let dynlink_argv = [DYNLINK_SMOKE_ARGV0.as_ptr(), ptr::null()];
            if let Err(code) = run_command_expect_output(
                "dynlink-smoke",
                DYNLINK_SMOKE_PATH,
                &dynlink_argv,
                DYNLINK_SMOKE_PASS_NEEDLE,
                &mut command_output,
            ) {
                return code;
            }
            pass("dynlink-smoke");
        }
    }

    // Phase 76b — full bring-up linker gate. Runs `/bin/dynlink_hello`
    // twice consecutively. The first run exercises the
    // PT_INTERP → ld.so self-relocation → DT_NEEDED → libhello.so map →
    // R_X86_64_JUMP_SLOT → main → hello_str() chain. The second run
    // verifies the refcount path on `libhello.so`.
    {
        let mut probe = Stat::zeroed();
        if stat(DYNLINK_HELLO_PATH, &mut probe) < 0 || probe.st_size == 0 {
            skip("dynlink-hello-smoke");
        } else {
            begin("dynlink-hello-smoke");
            let argv = [DYNLINK_HELLO_ARGV0.as_ptr(), ptr::null()];
            if let Err(code) = run_command_expect_output(
                "dynlink-hello-smoke",
                DYNLINK_HELLO_PATH,
                &argv,
                DYNLINK_HELLO_PASS_NEEDLE,
                &mut command_output,
            ) {
                return code;
            }
            if let Err(code) = run_command_expect_output(
                "dynlink-hello-smoke",
                DYNLINK_HELLO_PATH,
                &argv,
                DYNLINK_HELLO_PASS_NEEDLE,
                &mut command_output,
            ) {
                return code;
            }
            pass("dynlink-hello-smoke");
        }
    }

    // Phase 76b F1.4 — missing-dependency negative gate. The
    // bring-up linker tries to open `/usr/lib/libdoesnotexist.so`,
    // gets ENOENT from the kernel, and exits with code 2 (ENOENT).
    {
        let mut probe = Stat::zeroed();
        if stat(DYNLINK_MISSING_PATH, &mut probe) < 0 || probe.st_size == 0 {
            skip("dynlink-missing-smoke");
        } else {
            begin("dynlink-missing-smoke");
            let argv = [DYNLINK_MISSING_ARGV0.as_ptr(), ptr::null()];
            if let Err(code) = run_command_expect_exit(
                "dynlink-missing-smoke",
                DYNLINK_MISSING_PATH,
                &argv,
                2,
                &mut command_output,
            ) {
                return code;
            }
            pass("dynlink-missing-smoke");
        }
    }

    // Phase 76b F1.4 — circular-dependency negative gate. The
    // bring-up linker's topo_sort detects the libcyca ↔ libcycb
    // cycle and exits with code 80 (ELIBBAD).
    {
        let mut probe = Stat::zeroed();
        if stat(DYNLINK_CYCLE_PATH, &mut probe) < 0 || probe.st_size == 0 {
            skip("dynlink-cycle-smoke");
        } else {
            begin("dynlink-cycle-smoke");
            let argv = [DYNLINK_CYCLE_ARGV0.as_ptr(), ptr::null()];
            if let Err(code) = run_command_expect_exit(
                "dynlink-cycle-smoke",
                DYNLINK_CYCLE_PATH,
                &argv,
                80,
                &mut command_output,
            ) {
                return code;
            }
            pass("dynlink-cycle-smoke");
        }
    }

    // Phase 76d.F — `dynlink_hello_gnu` exercises the GNU-hash backend
    // (D1), the PLT lazy-resolve trampoline (B4), the `LD_BIND_NOW=1`
    // env-var path (E4/F.3), and the W^X invariant (F.4). Two-phase:
    //   * default env: lazy resolution (B4 trampoline path) → BIND_NOW:0.
    //   * `LD_BIND_NOW=1`: eager resolution (E4 env-var path, F.3) → BIND_NOW:1.
    // Both phases must emit `HELLO_FROM_GNU_LIB:OK` and `WX_CHECK:OK`
    // (F.4); the BIND_NOW:{0,1} sentinel distinguishes which resolution
    // mode the linker actually took. The F.2 "trampoline traversal"
    // assertion is implicit: under lazy default, the first call to
    // hello_str() can only print the sentinel by running through the
    // trampoline.
    {
        let mut probe = Stat::zeroed();
        if stat(DYNLINK_HELLO_GNU_PATH, &mut probe) < 0 || probe.st_size == 0 {
            skip("dynlink-hello-gnu-smoke");
        } else {
            begin("dynlink-hello-gnu-smoke");
            let argv = [DYNLINK_HELLO_GNU_ARGV0.as_ptr(), ptr::null()];
            // Phase 1 — default env: lazy resolution → BIND_NOW:0.
            let empty_envp = [ptr::null()];
            if let Err(code) = run_command_expect_outputs_with_env(
                "dynlink-hello-gnu-smoke",
                DYNLINK_HELLO_GNU_PATH,
                &argv,
                &empty_envp,
                &[
                    DYNLINK_HELLO_GNU_PASS_NEEDLE,
                    DYNLINK_HELLO_GNU_LAZY_NEEDLE,
                    DYNLINK_HELLO_GNU_WX_NEEDLE,
                ],
                &mut command_output,
            ) {
                return code;
            }
            // Phase 2 — LD_BIND_NOW=1: eager → BIND_NOW:1 (F.3).
            let env_bind_now = [LD_BIND_NOW_ENV.as_ptr(), ptr::null()];
            if let Err(code) = run_command_expect_outputs_with_env(
                "dynlink-hello-gnu-smoke",
                DYNLINK_HELLO_GNU_PATH,
                &argv,
                &env_bind_now,
                &[
                    DYNLINK_HELLO_GNU_PASS_NEEDLE,
                    DYNLINK_HELLO_GNU_EAGER_NEEDLE,
                    DYNLINK_HELLO_GNU_WX_NEEDLE,
                ],
                &mut command_output,
            ) {
                return code;
            }
            pass("dynlink-hello-gnu-smoke");
        }
    }

    // Phase 76d.G — versioned-symbol gate. Two-phase:
    //   * default env: lazy resolution, version-aware lookup in
    //     `sym::lookup` matches `LIBHELLO_1.0` against the lib's
    //     `DT_VERSYM` + `DT_VERDEF`.
    //   * `LD_BIND_NOW=1`: eager resolution + D2.3 strict mode.
    //     Exact-version match must still succeed (strict only fires
    //     on mismatch); this phase exercises the BIND_NOW=true
    //     branch through `sym::lookup`.
    {
        let mut probe = Stat::zeroed();
        if stat(DYNLINK_HELLO_VERSIONED_PATH, &mut probe) < 0 || probe.st_size == 0 {
            skip("dynlink-hello-versioned-smoke");
        } else {
            begin("dynlink-hello-versioned-smoke");
            let argv = [DYNLINK_HELLO_VERSIONED_ARGV0.as_ptr(), ptr::null()];
            // Phase 1 — lazy + default env.
            let empty_envp = [ptr::null()];
            if let Err(code) = run_command_expect_outputs_with_env(
                "dynlink-hello-versioned-smoke",
                DYNLINK_HELLO_VERSIONED_PATH,
                &argv,
                &empty_envp,
                &[DYNLINK_HELLO_VERSIONED_PASS_NEEDLE],
                &mut command_output,
            ) {
                return code;
            }
            // Phase 2 — eager + strict (LD_BIND_NOW=1). Exact match
            // still succeeds; this phase proves D2.3's strict gate
            // doesn't reject matching versions.
            let env_bind_now = [LD_BIND_NOW_ENV.as_ptr(), ptr::null()];
            if let Err(code) = run_command_expect_outputs_with_env(
                "dynlink-hello-versioned-smoke",
                DYNLINK_HELLO_VERSIONED_PATH,
                &argv,
                &env_bind_now,
                &[DYNLINK_HELLO_VERSIONED_PASS_NEEDLE],
                &mut command_output,
            ) {
                return code;
            }
            pass("dynlink-hello-versioned-smoke");
        }
    }

    // Phase 76d.G.3 — mismatch + LD_BIND_NOW strict-mode gate. The
    // consumer requires `LIBHELLO_1.0` against a lib (v2) that only
    // provides `LIBHELLO_2.0` plus an unversioned `hello_str`.
    //
    //   * Default mode (lazy + no env): D2.2 detects the mismatch,
    //     emits a serial warn, falls back to the unversioned
    //     `hello_str` → prints `HELLO_FROM_V2_FALLBACK:OK`.
    //   * Strict mode (LD_BIND_NOW=1): D2.3 detects the mismatch,
    //     emits a serial error, returns None. apply_rela errors and
    //     `dl_entry` returns 0; the asm caller `jmp 0` triggers a
    //     SIGSEGV — the smoke gate asserts a non-zero exit.
    {
        let mut probe = Stat::zeroed();
        if stat(DYNLINK_HELLO_VERSIONED_MISMATCH_PATH, &mut probe) < 0 || probe.st_size == 0 {
            skip("dynlink-hello-versioned-mismatch-smoke");
        } else {
            begin("dynlink-hello-versioned-mismatch-smoke");
            let argv = [DYNLINK_HELLO_VERSIONED_MISMATCH_ARGV0.as_ptr(), ptr::null()];
            // Phase 1 — default mode: fallback to unversioned.
            let empty_envp = [ptr::null()];
            if let Err(code) = run_command_expect_outputs_with_env(
                "dynlink-hello-versioned-mismatch-smoke",
                DYNLINK_HELLO_VERSIONED_MISMATCH_PATH,
                &argv,
                &empty_envp,
                &[DYNLINK_HELLO_VERSIONED_MISMATCH_FALLBACK_NEEDLE],
                &mut command_output,
            ) {
                return code;
            }
            // Phase 2 — strict mode: non-zero exit.
            let env_bind_now = [LD_BIND_NOW_ENV.as_ptr(), ptr::null()];
            if let Err(code) = run_command_expect_nonzero_with_env(
                "dynlink-hello-versioned-mismatch-smoke",
                DYNLINK_HELLO_VERSIONED_MISMATCH_PATH,
                &argv,
                &env_bind_now,
                &mut command_output,
            ) {
                return code;
            }
            pass("dynlink-hello-versioned-mismatch-smoke");
        }
    }

    // Phase 76c — full libdl runtime gate. Runs `/bin/dlopen_test`,
    // asserts every libdl entry point works (positive + four negative
    // paths) AND that the destructor pipeline runs in the correct
    // serial order. A missing `LIBHELLO_FINI:RAN` between the
    // `DLOPEN_TEST:FINI_PENDING` and `DLOPEN_TEST:PASS` bracket
    // sentinels is a hard FAIL.
    {
        let mut probe = Stat::zeroed();
        if stat(DLOPEN_TEST_PATH, &mut probe) < 0 || probe.st_size == 0 {
            skip("dlopen-test-smoke");
        } else {
            begin("dlopen-test-smoke");
            let argv = [DLOPEN_TEST_ARGV0.as_ptr(), ptr::null()];
            if let Err(code) = run_command_expect_dlopen_order(
                "dlopen-test-smoke",
                DLOPEN_TEST_PATH,
                &argv,
                &mut command_output,
            ) {
                return code;
            }
            pass("dlopen-test-smoke");
        }
    }

    write_str(STDOUT_FILENO, "SMOKE:PASS\n");
    0
}

/// Phase 76c-specific helper: run `dlopen_test` and assert the
/// captured output contains FINI_PENDING → LIBHELLO_FINI:RAN →
/// DLOPEN_TEST:PASS in strict serial order. Catches both a missing
/// destructor invocation AND a destructor that fired before the
/// `dlclose` call (which would imply the pipeline is running too
/// eagerly).
fn run_command_expect_dlopen_order(
    stage: &str,
    path: &[u8],
    argv: &[*const u8],
    output: &mut [u8],
) -> Result<(), i32> {
    let (status, len) = match run_command_capture(path, argv, output) {
        Ok(result) => result,
        Err(msg) => return Err(fail(stage, msg, 12)),
    };
    match exit_code(status) {
        Some(0) => {}
        Some(c) => {
            return Err(fail_with_output(
                stage,
                "dlopen_test exited non-zero",
                40 + c.max(0),
                &output[..len],
            ));
        }
        None => {
            return Err(fail_with_output(
                stage,
                "dlopen_test did not exit normally",
                49,
                &output[..len],
            ));
        }
    }
    let captured = &output[..len];
    let pending = find_subslice(captured, DLOPEN_TEST_PENDING_NEEDLE);
    let fini = find_subslice(captured, DLOPEN_TEST_FINI_RAN_NEEDLE);
    let done = find_subslice(captured, DLOPEN_TEST_PASS_NEEDLE);
    match (pending, fini, done) {
        (Some(p), Some(f), Some(d)) if p < f && f < d => Ok(()),
        (Some(_), None, Some(_)) => Err(fail_with_output(
            stage,
            "LIBHELLO_FINI:RAN missing between FINI_PENDING and PASS",
            50,
            captured,
        )),
        (None, _, _) => Err(fail_with_output(
            stage,
            "DLOPEN_TEST:FINI_PENDING sentinel missing",
            51,
            captured,
        )),
        (_, _, None) => Err(fail_with_output(
            stage,
            "DLOPEN_TEST:PASS sentinel missing",
            52,
            captured,
        )),
        _ => Err(fail_with_output(
            stage,
            "FINI_PENDING / LIBHELLO_FINI:RAN / PASS out of order",
            53,
            captured,
        )),
    }
}

/// Find `needle` in `haystack`; return the index of the first
/// occurrence or `None`. Hand-rolled because `slice::find` is unstable
/// for no_std consumers.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    let mut i = 0;
    while i <= last {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Run a command and assert it exits with `expected_code`. Used by
/// the Phase 76b F1.4 negative gates: missing-dep → exit(2),
/// cyclic-DT_NEEDED → exit(80).
fn run_command_expect_exit(
    stage: &str,
    path: &[u8],
    argv: &[*const u8],
    expected_code: i32,
    output: &mut [u8],
) -> Result<(), i32> {
    let (status, len) = match run_command_capture(path, argv, output) {
        Ok(result) => result,
        Err(msg) => return Err(fail(stage, msg, 12)),
    };
    match exit_code(status) {
        Some(c) if c == expected_code => Ok(()),
        Some(c) => Err(fail_with_output(
            stage,
            "unexpected exit code",
            18 + c,
            &output[..len],
        )),
        None => Err(fail_with_output(
            stage,
            "process did not exit normally",
            19,
            &output[..len],
        )),
    }
}

/// Phase 76d.G.3 — assert the command exits with ANY non-zero code
/// (or did not exit normally — e.g., killed by SIGSEGV after the
/// linker's `dl_entry` returns 0 from a strict-mode mismatch). Takes
/// a caller-provided `envp` so the gate can set `LD_BIND_NOW=1`.
fn run_command_expect_nonzero_with_env(
    stage: &str,
    path: &[u8],
    argv: &[*const u8],
    envp: &[*const u8],
    output: &mut [u8],
) -> Result<(), i32> {
    let (status, len) = match run_command_capture_with_env(path, argv, envp, output) {
        Ok(result) => result,
        Err(msg) => return Err(fail(stage, msg, 12)),
    };
    match exit_code(status) {
        Some(0) => Err(fail_with_output(
            stage,
            "expected non-zero exit but command succeeded",
            20,
            &output[..len],
        )),
        // Either non-zero exit (e.g. linker exited with code 80) or
        // killed by a signal (e.g. SIGSEGV from jumping to 0 after
        // dl_entry returned 0). Both count as failure-as-expected.
        _ => Ok(()),
    }
}

fn verify_required_storage_files() -> Result<(), i32> {
    let mut meta = Stat::zeroed();
    if stat(PASSWD_PATH, &mut meta) < 0 || meta.st_size == 0 {
        return Err(fail("storage", "stat(/etc/passwd) failed", 5));
    }
    if stat(HELLO_SOURCE_PATH, &mut meta) < 0 || meta.st_size == 0 {
        return Err(fail("storage", "stat(/usr/src/hello.c) failed", 6));
    }

    Ok(())
}

fn verify_compiled_hello() -> Result<(), i32> {
    let mut meta = Stat::zeroed();
    if stat(HELLO_BIN_PATH, &mut meta) < 0 {
        return Err(fail("hello", "stat(/tmp/h) failed", 15));
    }
    if meta.st_size == 0 {
        let _ = unlink(HELLO_BIN_PATH);
        return Err(fail("hello", "compiled hello binary is empty", 16));
    }
    if unlink(HELLO_BIN_PATH) < 0 {
        return Err(fail("hello", "unlink(/tmp/h) failed", 17));
    }
    Ok(())
}

fn inject_and_verify_log_marker(command_output: &mut [u8]) -> Result<(), i32> {
    let mut marker_buf = [0u8; 64];
    let marker_len = build_log_marker(&mut marker_buf);
    if marker_len == 0 {
        return Err(fail("log", "failed to build log marker", 8));
    }
    let marker = &marker_buf[..marker_len];
    let marker_cstr = &marker_buf[..marker_len + 1];

    let logger_argv = [LOGGER_ARGV0.as_ptr(), marker_cstr.as_ptr(), ptr::null()];
    run_command_expect_success("log", LOGGER_PATH, &logger_argv, command_output)?;

    // Poll at 100 ms intervals for up to ~15 s. The previous 1-second
    // cadence wasted up to 900 ms after a marker arrived between polls;
    // tighter granularity catches the marker on the first read after
    // syslogd writes (which now `fsync`s, see
    // userspace/syslogd/src/main.rs).
    if !wait_for_file_contains(SYSTEM_LOG_PATH, marker, 150) {
        return Err(fail("log", "marker missing from /var/log/messages", 9));
    }

    Ok(())
}

fn build_log_marker(buf: &mut [u8]) -> usize {
    let prefix = b"SMOKE_LOG_MARKER_";
    if buf.len() <= prefix.len() + 1 {
        return 0;
    }
    buf[..prefix.len()].copy_from_slice(prefix);
    let mut len = prefix.len();
    let pid = getpid();
    let pid = if pid < 0 { 0 } else { pid as u64 };
    let digits_end = buf.len() - 1;
    len += write_decimal_into(&mut buf[len..digits_end], pid);
    buf[len] = 0;
    len
}

fn write_decimal_into(buf: &mut [u8], mut n: u64) -> usize {
    if buf.is_empty() {
        return 0;
    }
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }

    let mut tmp = [0u8; 20];
    let mut pos = tmp.len();
    while n > 0 {
        pos -= 1;
        tmp[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let digits = tmp.len() - pos;
    if digits > buf.len() {
        return 0;
    }
    buf[..digits].copy_from_slice(&tmp[pos..]);
    digits
}

/// Poll `path` for `needle`, sleeping 100 ms between attempts.
///
/// Total budget = `attempts × 100 ms`. The fine-grained sleep matters for
/// the syslog-marker check (see `inject_and_verify_log_marker`) where the
/// writer's flush completes asynchronously and we want to observe it on
/// the next read rather than wait up to 1 s for the next poll cycle.
fn wait_for_file_contains(path: &[u8], needle: &[u8], attempts: usize) -> bool {
    const SLEEP_NS: u32 = 100_000_000; // 100 ms
    for _ in 0..attempts {
        if let Ok(found) = file_contains(path, needle)
            && found
        {
            return true;
        }
        let _ = syscall_lib::nanosleep_for(0, SLEEP_NS);
    }
    false
}

fn file_contains(path: &[u8], needle: &[u8]) -> Result<bool, ()> {
    if needle.is_empty() {
        return Ok(true);
    }

    let fd = open(path, O_RDONLY, 0);
    if fd < 0 {
        return Err(());
    }

    let fd = fd as i32;
    let mut scan_buf = [0u8; FILE_SCAN_BUF_LEN];
    let mut carry_len = 0usize;
    loop {
        let n = read(fd, &mut scan_buf[carry_len..]);
        if n < 0 {
            let _ = close(fd);
            return Err(());
        }
        if n == 0 {
            break;
        }
        let total = carry_len + n as usize;
        if contains_bytes(&scan_buf[..total], needle) {
            let _ = close(fd);
            return Ok(true);
        }

        let keep = core::cmp::min(needle.len().saturating_sub(1), total);
        scan_buf.copy_within(total - keep..total, 0);
        carry_len = keep;
    }
    let _ = close(fd);
    Ok(false)
}

fn run_command_expect_success(
    stage: &str,
    path: &[u8],
    argv: &[*const u8],
    _output: &mut [u8],
) -> Result<(), i32> {
    let status = match run_command_wait(path, argv) {
        Ok(status) => status,
        Err(msg) => return Err(fail(stage, msg, 10)),
    };

    if exit_code(status) != Some(0) {
        return Err(fail(stage, "command exited non-zero", 11));
    }

    Ok(())
}

fn run_command_wait(path: &[u8], argv: &[*const u8]) -> Result<i32, &'static str> {
    let pid = fork();
    if pid < 0 {
        return Err("fork() failed");
    }

    if pid == 0 {
        let envp = [ptr::null()];
        let _ = execve(path, argv, &envp);
        write_str(STDOUT_FILENO, "execve() failed\n");
        exit(127);
    }

    let mut status = 0i32;
    if waitpid(pid as i32, &mut status, 0) != pid as isize {
        return Err("waitpid() failed");
    }

    Ok(status)
}

fn run_command_expect_output(
    stage: &str,
    path: &[u8],
    argv: &[*const u8],
    needle: &[u8],
    output: &mut [u8],
) -> Result<(), i32> {
    let (status, len) = match run_command_capture(path, argv, output) {
        Ok(result) => result,
        Err(msg) => return Err(fail(stage, msg, 12)),
    };

    if exit_code(status) != Some(0) {
        return Err(fail_with_output(
            stage,
            "command exited non-zero",
            13,
            &output[..len],
        ));
    }

    if !contains_bytes(&output[..len], needle) {
        return Err(fail_with_output(
            stage,
            "expected output marker missing",
            14,
            &output[..len],
        ));
    }

    Ok(())
}

/// Phase 76d.F — run a command and assert that EVERY one of the
/// listed `needles` appears in its stdout/stderr capture, with an
/// optional environment vector (`envp`).
///
/// `envp` is the SysV envp shape: each entry is a pointer to a
/// NUL-terminated `KEY=VAL\0` string; the slice itself ends with a
/// null pointer. Pass `&[ptr::null()]` for the empty environment
/// (matches `run_command_expect_output`).
fn run_command_expect_outputs_with_env(
    stage: &str,
    path: &[u8],
    argv: &[*const u8],
    envp: &[*const u8],
    needles: &[&[u8]],
    output: &mut [u8],
) -> Result<(), i32> {
    let (status, len) = match run_command_capture_with_env(path, argv, envp, output) {
        Ok(result) => result,
        Err(msg) => return Err(fail(stage, msg, 12)),
    };
    if exit_code(status) != Some(0) {
        return Err(fail_with_output(
            stage,
            "command exited non-zero",
            13,
            &output[..len],
        ));
    }
    for needle in needles {
        if !contains_bytes(&output[..len], needle) {
            return Err(fail_with_output(
                stage,
                "expected output marker missing",
                14,
                &output[..len],
            ));
        }
    }
    Ok(())
}

fn run_command_capture(
    path: &[u8],
    argv: &[*const u8],
    buf: &mut [u8],
) -> Result<(i32, usize), &'static str> {
    let capture_fd = open(CAPTURE_FILE_PATH, O_WRONLY | O_CREAT | O_TRUNC, 0o600);
    if capture_fd < 0 {
        return Err("open(capture file) failed");
    }
    let capture_fd = capture_fd as i32;

    let pid = fork();
    if pid < 0 {
        let _ = close(capture_fd);
        return Err("fork() failed");
    }

    if pid == 0 {
        if dup2(capture_fd, STDOUT_FILENO) < 0 || dup2(capture_fd, STDERR_FILENO) < 0 {
            exit(126);
        }
        let _ = close(capture_fd);
        let envp = [ptr::null()];
        let _ = execve(path, argv, &envp);
        write_str(STDOUT_FILENO, "execve() failed\n");
        exit(127);
    }

    let _ = close(capture_fd);

    let mut status = 0i32;
    if waitpid(pid as i32, &mut status, 0) != pid as isize {
        let _ = unlink(CAPTURE_FILE_PATH);
        return Err("waitpid() failed");
    }

    let capture_fd = open(CAPTURE_FILE_PATH, O_RDONLY, 0);
    if capture_fd < 0 {
        let _ = unlink(CAPTURE_FILE_PATH);
        return Err("open(capture file for read) failed");
    }
    let capture_fd = capture_fd as i32;

    let mut total = 0usize;
    let mut discard = [0u8; 256];
    loop {
        let read_buf = if total < buf.len() {
            &mut buf[total..]
        } else {
            &mut discard[..]
        };

        let n = read(capture_fd, read_buf);
        if n < 0 {
            let _ = close(capture_fd);
            let _ = unlink(CAPTURE_FILE_PATH);
            return Err("read(capture file) failed");
        }
        if n == 0 {
            break;
        }
        if total < buf.len() {
            total += n as usize;
        }
    }

    let _ = close(capture_fd);
    let _ = unlink(CAPTURE_FILE_PATH);

    Ok((status, total.min(buf.len())))
}

/// Phase 76d.F.3 — same as `run_command_capture` but accepts a
/// caller-provided `envp` slice so the smoke gate can drive
/// `LD_BIND_NOW=1`. The slice must follow the SysV envp convention
/// (each entry a NUL-terminated `KEY=VAL\0`, last entry null).
fn run_command_capture_with_env(
    path: &[u8],
    argv: &[*const u8],
    envp: &[*const u8],
    buf: &mut [u8],
) -> Result<(i32, usize), &'static str> {
    let capture_fd = open(CAPTURE_FILE_PATH, O_WRONLY | O_CREAT | O_TRUNC, 0o600);
    if capture_fd < 0 {
        return Err("open(capture file) failed");
    }
    let capture_fd = capture_fd as i32;

    let pid = fork();
    if pid < 0 {
        let _ = close(capture_fd);
        return Err("fork() failed");
    }

    if pid == 0 {
        if dup2(capture_fd, STDOUT_FILENO) < 0 || dup2(capture_fd, STDERR_FILENO) < 0 {
            exit(126);
        }
        let _ = close(capture_fd);
        let _ = execve(path, argv, envp);
        write_str(STDOUT_FILENO, "execve() failed\n");
        exit(127);
    }

    let _ = close(capture_fd);

    let mut status = 0i32;
    if waitpid(pid as i32, &mut status, 0) != pid as isize {
        let _ = unlink(CAPTURE_FILE_PATH);
        return Err("waitpid() failed");
    }

    let capture_fd = open(CAPTURE_FILE_PATH, O_RDONLY, 0);
    if capture_fd < 0 {
        let _ = unlink(CAPTURE_FILE_PATH);
        return Err("open(capture file for read) failed");
    }
    let capture_fd = capture_fd as i32;

    let mut total = 0usize;
    let mut discard = [0u8; 256];
    loop {
        let read_buf = if total < buf.len() {
            &mut buf[total..]
        } else {
            &mut discard[..]
        };
        let n = read(capture_fd, read_buf);
        if n < 0 {
            let _ = close(capture_fd);
            let _ = unlink(CAPTURE_FILE_PATH);
            return Err("read(capture file) failed");
        }
        if n == 0 {
            break;
        }
        if total < buf.len() {
            total += n as usize;
        }
    }

    let _ = close(capture_fd);
    let _ = unlink(CAPTURE_FILE_PATH);

    Ok((status, total.min(buf.len())))
}

fn exit_code(status: i32) -> Option<i32> {
    if (status & 0x7f) == 0 {
        Some((status >> 8) & 0xff)
    } else {
        None
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Emit `SMOKE:<stage>:<verdict>\n` (optionally with a trailing message)
/// as a single `write()` syscall.
///
/// The smoke-test harness pattern-matches on these literal lines. Under
/// fast (KVM) timing, multiple `write_str()` calls can be sliced apart by
/// other userspace processes that share the serial port (init's
/// smoke-runner pid log; display_server's compose log; etc.) — every
/// userspace `write()` only takes SERIAL1 for its own bytes, so a kernel
/// IRQ-context log or another userspace `write()` between two of our
/// writes leaves a sliced line on the wire (`SMOKE:5auth19:PASS`).
/// Single `write()` per logical line eliminates the slice surface for the
/// patterns the harness depends on.
fn write_marker(stage: &str, verdict: &str, msg: Option<&str>) {
    let mut buf = [0u8; 256];
    let mut total = 0usize;
    let prefix = b"SMOKE:";
    let stage_b = stage.as_bytes();
    let verdict_b = verdict.as_bytes();
    let msg_b = msg.unwrap_or("").as_bytes();

    let need = prefix.len() + stage_b.len() + 1 + verdict_b.len() + msg_b.len() + 1;
    if need > buf.len() {
        // Fallback to the legacy multi-write path if a message is too long
        // to fit; long-message verdicts are rare and the diagnostic value
        // outweighs the small interleave risk.
        write_str(STDOUT_FILENO, "SMOKE:");
        write_str(STDOUT_FILENO, stage);
        write_str(STDOUT_FILENO, ":");
        write_str(STDOUT_FILENO, verdict);
        if let Some(m) = msg
            && !m.is_empty()
        {
            write_str(STDOUT_FILENO, m);
        }
        write_str(STDOUT_FILENO, "\n");
        return;
    }
    buf[..prefix.len()].copy_from_slice(prefix);
    total += prefix.len();
    buf[total..total + stage_b.len()].copy_from_slice(stage_b);
    total += stage_b.len();
    buf[total] = b':';
    total += 1;
    buf[total..total + verdict_b.len()].copy_from_slice(verdict_b);
    total += verdict_b.len();
    if !msg_b.is_empty() {
        buf[total..total + msg_b.len()].copy_from_slice(msg_b);
        total += msg_b.len();
    }
    buf[total] = b'\n';
    total += 1;
    let _ = syscall_lib::write(STDOUT_FILENO, &buf[..total]);
}

fn pass(stage: &str) {
    write_marker(stage, "PASS", None);
}

fn begin(stage: &str) {
    write_marker(stage, "BEGIN", None);
}

fn skip(stage: &str) {
    write_marker(stage, "SKIP", None);
}

fn marker_present(path: &[u8]) -> bool {
    let mut meta = Stat::zeroed();
    stat(path, &mut meta) >= 0
}

fn fail(stage: &str, msg: &str, code: i32) -> i32 {
    // Format `SMOKE:<stage>:FAIL <msg>` as a single write. See `write_marker`
    // for why this matters under fast (KVM) timing.
    let mut joined_buf = [0u8; 256];
    let space = b" ";
    let total_msg_len = space.len() + msg.len();
    if total_msg_len + 32 < joined_buf.len() {
        joined_buf[..space.len()].copy_from_slice(space);
        joined_buf[space.len()..space.len() + msg.len()].copy_from_slice(msg.as_bytes());
        // SAFETY: bytes written above are ASCII (space + msg's caller-provided text).
        let joined = unsafe { core::str::from_utf8_unchecked(&joined_buf[..total_msg_len]) };
        write_marker(stage, "FAIL", Some(joined));
    } else {
        write_marker(stage, "FAIL", None);
    }
    code
}

fn fail_with_output(stage: &str, msg: &str, code: i32, output: &[u8]) -> i32 {
    let code = fail(stage, msg, code);
    if !output.is_empty() {
        write_str(STDOUT_FILENO, "SMOKE:output:");
        let _ = write(STDOUT_FILENO, output);
        if !output.ends_with(b"\n") {
            write_str(STDOUT_FILENO, "\n");
        }
    }
    code
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "SMOKE:panic:FAIL\n");
    exit(101)
}
