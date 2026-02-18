# Data Model: Kernel Boot Foundation

This is a bare-metal OS kernel — there are no database entities or API resources. The "data model" describes the key structures that flow through the system at boot time.

## Entities

### BootInfo (external, read-only)

Provided by the `bootloader_api` crate. Passed to `kernel_main` at boot.

| Field | Type | Description |
|---|---|---|
| `memory_regions` | `&[MemoryRegion]` | Physical memory map from firmware |
| `physical_memory_offset` | `Option<u64>` | Base of identity-mapped physical memory |
| `framebuffer` | `Option<FrameBuffer>` | Pixel buffer info (unused in Phase 1) |
| `rsdp_addr` | `Option<u64>` | ACPI root pointer (unused in Phase 1) |

**Phase 1 usage**: `BootInfo` is received but not consumed. No fields are accessed until Phase 2 (memory management). The parameter is kept for forward compatibility.

### SerialPort (singleton)

Global serial port instance, initialized once at boot.

| Field | Type | Description |
|---|---|---|
| `port` | `uart_16550::SerialPort` | Wrapped COM1 UART at 0x3F8 |
| `mutex` | `spin::Mutex` | Protects concurrent access |

**Lifecycle**: Created as a global static. Initialized in `kernel_main` before any output. Never dropped.

### Logger (singleton)

Zero-sized struct implementing `log::Log` trait.

| Method | Behavior |
|---|---|
| `enabled()` | Always returns `true` |
| `log()` | Formats `[LEVEL] message` and writes via `serial_println!` |
| `flush()` | No-op (serial is unbuffered) |

**Lifecycle**: Set as global logger via `log::set_logger()` during serial initialization.

## Relationships

```text
BootInfo ──(passed to)──> kernel_main
                              │
                              ├── init SerialPort (global static)
                              ├── init Logger (uses SerialPort)
                              ├── serial_println!("[ostest] Hello from kernel!")
                              ├── log::info!("Kernel initialized")
                              └── hlt_loop()
```

## State Transitions

The kernel has a single linear state machine in Phase 1:

```text
[Boot] → [Serial Init] → [Logger Init] → [Hello Message] → [HLT Loop]
                                                                 │
                                                          (panic at any point)
                                                                 │
                                                          [Panic Handler] → [HLT Loop]
```

No state transitions after reaching HLT loop. The system is idle until externally terminated.
