//! Phase 84 D.2 — make the `mitigations=` build-time default rebuild-aware.
//!
//! The kernel reads `option_env!("M3OS_MITIGATIONS")` at compile time (the
//! build-time `mitigations=off|auto|full` default; absent → `auto`). m3OS has
//! no kernel boot cmdline (`bootloader_api::BootInfo` carries none), so this is
//! how the level is selected. Tell cargo to recompile the kernel when that env
//! changes, so the xtask spectre / perf gates can flip the level by setting it.

fn main() {
    println!("cargo:rerun-if-env-changed=M3OS_MITIGATIONS");
}
