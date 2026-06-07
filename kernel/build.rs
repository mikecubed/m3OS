//! Phase 84 D.2 — make the `mitigations=` build-time default rebuild-aware.
//!
//! The kernel reads `option_env!("M3OS_MITIGATIONS")` at compile time (the
//! build-time `mitigations=off|auto|full` default; absent → `auto`). m3OS has
//! no kernel boot cmdline (`bootloader_api::BootInfo` carries none), so this is
//! how the level is selected. Tell cargo to recompile the kernel when that env
//! changes, so the xtask spectre / perf gates can flip the level by setting it.
//!
//! Phase 86a Track B — emit `M3OS_BUILD_EPOCH` for the wall-clock floor.
//!
//! If `SOURCE_DATE_EPOCH` is set (reproducible-build convention), its value is
//! used verbatim.  Otherwise the current wall-clock at build time is used.  The
//! epoch is consumed by `kernel/src/rtc.rs` via `option_env!("M3OS_BUILD_EPOCH")`
//! to guarantee `BOOT_EPOCH_SECS` never stays at zero on an invalid RTC.

fn main() {
    println!("cargo:rerun-if-env-changed=M3OS_MITIGATIONS");

    // --- build-date floor (Phase 86a Track B) ---
    //
    // Respect SOURCE_DATE_EPOCH for reproducible builds; fall back to the
    // current wall-clock.  Intentionally no `rerun-if-changed` for the
    // fallback path: the floor is computed once per build and does not need to
    // force a rebuild on every invocation.
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let secs: u64 = if let Ok(val) = std::env::var("SOURCE_DATE_EPOCH") {
        val.trim()
            .parse()
            .expect("SOURCE_DATE_EPOCH must be a non-negative integer (Unix seconds)")
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time is before the Unix epoch")
            .as_secs()
    };

    println!("cargo:rustc-env=M3OS_BUILD_EPOCH={secs}");
}
