# ACPI test fixtures

- `qemu-q35-dsdt.aml` — the reference DSDT QEMU generates for the q35
  machine type, taken from the QEMU source tree's expected-table test
  data (`tests/data/acpi/x86/q35/DSDT`, QEMU is GPLv2). This is the same
  namespace the Phase 101 QEMU `acpi-smoke` gate will see at runtime, so
  the host tests and the in-VM gate exercise one firmware image.

- **Pending (next Dell session):** `dell-5560-dsdt.aml` + SSDTs captured
  from the reference Dell Precision 5560 (Tiger Lake) via `acpidump` —
  see `docs/handoffs/next-dell-session.md`. Once landed, the
  `find_by_hid("DLL0945")` and touchpad-`_CRS` tests in
  `tests/acpi_qemu_dsdt.rs` gain real-silicon arms per the Phase 101
  charter (Track F).
