# Third-Party Software Notices

m3OS includes or depends on the following third-party software. Each
component is used under its respective license terms.

## Directly Integrated Components

### Ion Shell

- **Source:** https://github.com/redox-os/ion
- **License:** MIT
- **Copyright:** Copyright (c) 2017 Redox OS Developers
- **Usage:** Compiled from source and embedded in the initrd as `/bin/ion`.

### Kibi (editor concepts)

- **Source:** https://github.com/ilai-deutel/kibi
- **License:** MIT OR Apache-2.0
- **Copyright:** Copyright (c) 2020 Ilai Deutel
- **Usage:** The `edit` binary (`userspace/edit/`) is a clean-room
  implementation inspired by kibi's architecture (itself based on
  antirez's kilo). No source code was copied; the editor was written
  from scratch using the same design patterns (line-based buffer, VT100
  escape sequences, append buffer, incremental search).

### Kilo (editor concepts)

- **Source:** https://github.com/antirez/kilo
- **License:** BSD 2-Clause
- **Copyright:** Copyright (c) 2016, Salvatore Sanfilippo
- **Usage:** Indirect influence via kibi. Same design patterns used.

### musl libc

- **Source:** https://musl.libc.org/
- **License:** MIT
- **Copyright:** Copyright (c) 2005-2020 Rich Felker, et al.
- **Usage:** C userspace binaries (`userspace/coreutils/`, `userspace/hello-c/`,
  etc.) are statically linked against musl via `musl-gcc`.

## Rust Crate Dependencies

All Rust crate dependencies are listed in `Cargo.lock`. The following
table covers direct dependencies of the kernel and build system. All
are licensed under MIT, Apache-2.0, or both.

| Crate | Version | License | Purpose |
|---|---|---|---|
| `bootloader` | 0.11.15 | MIT OR Apache-2.0 | UEFI bootloader |
| `bootloader_api` | 0.11.15 | MIT OR Apache-2.0 | Kernel entry point, BootInfo |
| `x86_64` | 0.15.4 | MIT/Apache-2.0 | Page tables, IDT, GDT, port I/O |
| `uart_16550` | 0.4.0 | MIT | Serial port driver |
| `pic8259` | 0.11.0 | Apache-2.0/MIT | 8259 PIC initialization |
| `linked_list_allocator` | 0.10.5 | Apache-2.0/MIT | Kernel heap allocator |
| `log` | 0.4.29 | MIT OR Apache-2.0 | Logging facade |
| `spin` | 0.9.8 | MIT | no_std Mutex/RwLock |
| `fatfs` | 0.3.6 | MIT | FAT32 filesystem (xtask) |
| `gpt` | 3.1.0 | MIT | GUID partition table (xtask) |
| `anyhow` | 1.0.101 | MIT OR Apache-2.0 | Error handling (xtask) |
| `serde_json` | 1.0.149 | MIT OR Apache-2.0 | JSON parsing (xtask) |
| `tempfile` | 3.25.0 | MIT OR Apache-2.0 | Temporary files (xtask) |

For the complete list of transitive dependencies and their licenses,
run:

```bash
cargo metadata --format-version 1 | python3 -c "
import json, sys
data = json.load(sys.stdin)
for pkg in sorted(data['packages'], key=lambda p: p['name']):
    print(f\"{pkg['name']} {pkg['version']} -- {pkg.get('license', 'unknown')}\")
"
```

## License Compatibility

m3OS is licensed under the MIT License. All third-party components used
are licensed under MIT, Apache-2.0, BSD 2-Clause, Unlicense, or Zlib —
all of which are compatible with MIT.
