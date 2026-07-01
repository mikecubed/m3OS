//! `_CRS` resource-descriptor decoding (Phase 101 Track C).
//!
//! Decodes the byte stream a `_CRS` evaluation yields (ACPI 6.5 §6.4:
//! small + large resource items terminated by an End Tag) into the typed
//! [`DeviceResources`] drivers consume. The descriptors that matter for
//! the laptop bring-up get full decodes:
//!
//! - **I2C SerialBus** (§6.4.3.8.2.1) — slave address, speed, and the
//!   `ResourceSource` path of the owning controller: what the Phase 102
//!   I2C-HID touchpad driver attaches with.
//! - **GpioInt / GpioIo** (§6.4.3.8.1) — pin numbers, edge/level,
//!   polarity, and the GPIO controller `ResourceSource`.
//! - **Interrupt / IRQ / IO / Memory32Fixed / FixedMemory** — the
//!   classic descriptors (EC, UARTs, legacy devices).
//!
//! Address-space descriptors (Word/DWord/QWord) get a basic min/max/len
//! decode. Anything else is preserved as [`ResourceItem::Unknown`] so
//! callers can see what was skipped. Malformed input returns
//! [`AmlError`]; nothing panics.

use alloc::string::String;
use alloc::vec::Vec;

use super::aml::object::AmlError;

/// Trigger mode for interrupts (GPIO or Extended Interrupt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Level,
    Edge,
}

/// Active polarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    ActiveHigh,
    ActiveLow,
    ActiveBoth,
}

/// One decoded `_CRS` entry.
#[derive(Debug, Clone, PartialEq)]
pub enum ResourceItem {
    /// Small IRQ descriptor (mask form).
    Irq {
        /// IRQ numbers (decoded from the 16-bit mask).
        irqs: Vec<u8>,
        trigger: Trigger,
        polarity: Polarity,
        shared: bool,
        wake: bool,
    },
    /// Large Extended Interrupt descriptor (GSI numbers).
    Interrupt {
        gsis: Vec<u32>,
        trigger: Trigger,
        polarity: Polarity,
        shared: bool,
        wake: bool,
        /// True when the descriptor describes a consumed interrupt (bit 0
        /// of the flags), false for a producer.
        consumer: bool,
    },
    /// Small IO port descriptor.
    Io {
        min: u16,
        max: u16,
        align: u8,
        len: u8,
        /// 16-bit decode when true (bit 0 of the info byte).
        decode16: bool,
    },
    /// Small Fixed IO descriptor.
    FixedIo {
        base: u16,
        len: u8,
    },
    Memory32Fixed {
        base: u32,
        len: u32,
        writable: bool,
    },
    /// Word/DWord/QWord address-space descriptor, basic decode.
    Address {
        /// 0 = memory, 1 = IO, 2 = bus number, other = vendor.
        resource_type: u8,
        min: u64,
        max: u64,
        len: u64,
        translation: u64,
    },
    I2cSerialBus {
        /// 7- or 10-bit slave address.
        address: u16,
        /// Connection speed in Hz.
        speed_hz: u32,
        /// True for 10-bit addressing.
        ten_bit: bool,
        /// True when the device is bus slave (bit meaning per spec:
        /// SlaveMode — virtually always true for `_CRS` consumers).
        slave: bool,
        /// Namespace path of the I2C controller this connection rides.
        source: String,
    },
    GpioInt {
        pins: Vec<u16>,
        trigger: Trigger,
        polarity: Polarity,
        shared: bool,
        wake: bool,
        /// PinConfig field (0 default, 1 pull-up, 2 pull-down, 3 none).
        pin_config: u8,
        /// Debounce timeout in units of 10 µs.
        debounce: u16,
        /// Namespace path of the GPIO controller.
        source: String,
    },
    GpioIo {
        pins: Vec<u16>,
        shared: bool,
        pin_config: u8,
        debounce: u16,
        source: String,
    },
    /// SPI/UART serial bus connections — recorded, not yet decoded.
    SerialBusOther {
        bus_type: u8,
        source: String,
    },
    /// Preserved-but-undecoded descriptor.
    Unknown {
        tag: u8,
    },
    /// Vendor-defined (small type 0x0E or large 0x84).
    Vendor {
        bytes: Vec<u8>,
    },
}

/// The typed result of decoding a `_CRS` buffer.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DeviceResources {
    pub items: Vec<ResourceItem>,
}

impl DeviceResources {
    /// The first I2C connection, if any — the "what bus/address is this
    /// device on" answer for I2C-HID.
    pub fn i2c(&self) -> Option<&ResourceItem> {
        self.items
            .iter()
            .find(|i| matches!(i, ResourceItem::I2cSerialBus { .. }))
    }

    /// The first GpioInt, if any — the interrupt line for I2C-HID.
    pub fn gpio_int(&self) -> Option<&ResourceItem> {
        self.items
            .iter()
            .find(|i| matches!(i, ResourceItem::GpioInt { .. }))
    }

    /// All GSIs from Extended Interrupt descriptors plus legacy IRQs.
    pub fn interrupts(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for item in &self.items {
            match item {
                ResourceItem::Interrupt { gsis, .. } => out.extend_from_slice(gsis),
                ResourceItem::Irq { irqs, .. } => {
                    out.extend(irqs.iter().map(|&i| i as u32));
                }
                _ => {}
            }
        }
        out
    }
}

// Small-item type codes (byte >> 3, bit 7 clear). DMA (0x05) and the
// Start/EndDependentFn tags fall through to `Unknown`.
const SMALL_IRQ: u8 = 0x04;
const SMALL_IO: u8 = 0x08;
const SMALL_FIXED_IO: u8 = 0x09;
const SMALL_VENDOR: u8 = 0x0E;
const SMALL_END_TAG: u8 = 0x0F;

// Large-item type codes (low 7 bits, bit 7 set).
const LARGE_MEMORY32_FIXED: u8 = 0x06;
const LARGE_ADDR32: u8 = 0x07;
const LARGE_ADDR16: u8 = 0x08;
const LARGE_EXT_IRQ: u8 = 0x09;
const LARGE_ADDR64: u8 = 0x0A;
const LARGE_EXT_ADDR64: u8 = 0x0B;
const LARGE_GPIO: u8 = 0x0C;
const LARGE_SERIAL_BUS: u8 = 0x0E;
const LARGE_VENDOR: u8 = 0x04;

fn get(bytes: &[u8], i: usize) -> Result<u8, AmlError> {
    bytes.get(i).copied().ok_or(AmlError::Truncated)
}

fn get_u16(bytes: &[u8], i: usize) -> Result<u16, AmlError> {
    Ok(u16::from_le_bytes([get(bytes, i)?, get(bytes, i + 1)?]))
}

fn get_u32(bytes: &[u8], i: usize) -> Result<u32, AmlError> {
    Ok(u32::from_le_bytes([
        get(bytes, i)?,
        get(bytes, i + 1)?,
        get(bytes, i + 2)?,
        get(bytes, i + 3)?,
    ]))
}

fn get_u64(bytes: &[u8], i: usize) -> Result<u64, AmlError> {
    Ok(u64::from_le_bytes([
        get(bytes, i)?,
        get(bytes, i + 1)?,
        get(bytes, i + 2)?,
        get(bytes, i + 3)?,
        get(bytes, i + 4)?,
        get(bytes, i + 5)?,
        get(bytes, i + 6)?,
        get(bytes, i + 7)?,
    ]))
}

/// ASCII `ResourceSource` string starting at `from` (NUL-terminated or
/// running to the end of the descriptor body).
fn source_string(body: &[u8], from: usize) -> String {
    let mut s = String::new();
    if from >= body.len() {
        return s;
    }
    for &b in &body[from..] {
        if b == 0 {
            break;
        }
        s.push(b as char);
    }
    s
}

/// Decode a `_CRS`/`_PRS` resource-template buffer.
///
/// Stops cleanly at the End Tag; trailing bytes after it are ignored
/// (firmware routinely pads). A missing End Tag decodes what is present.
pub fn decode_crs(bytes: &[u8]) -> Result<DeviceResources, AmlError> {
    let mut out = DeviceResources::default();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let head = bytes[pos];
        if head & 0x80 == 0 {
            // Small item: type in bits 6:3, length in bits 2:0.
            let ty = (head >> 3) & 0x0F;
            let len = (head & 0x07) as usize;
            let body_start = pos + 1;
            if body_start + len > bytes.len() {
                return Err(AmlError::Truncated);
            }
            let body = &bytes[body_start..body_start + len];
            match ty {
                SMALL_END_TAG => return Ok(out),
                SMALL_IRQ => {
                    let mask = get_u16(body, 0)?;
                    let irqs: Vec<u8> = (0..16).filter(|b| mask & (1 << b) != 0).collect();
                    // Optional third byte carries the flags; its absence
                    // means the ISA default: edge, active-high, exclusive.
                    let flags = if len >= 3 { body[2] } else { 0x01 };
                    out.items.push(ResourceItem::Irq {
                        irqs,
                        trigger: if flags & 0x01 != 0 {
                            Trigger::Edge
                        } else {
                            Trigger::Level
                        },
                        polarity: if flags & 0x08 != 0 {
                            Polarity::ActiveLow
                        } else {
                            Polarity::ActiveHigh
                        },
                        shared: flags & 0x10 != 0,
                        wake: flags & 0x20 != 0,
                    });
                }
                SMALL_IO => {
                    out.items.push(ResourceItem::Io {
                        decode16: get(body, 0)? & 0x01 != 0,
                        min: get_u16(body, 1)?,
                        max: get_u16(body, 3)?,
                        align: get(body, 5)?,
                        len: get(body, 6)?,
                    });
                }
                SMALL_FIXED_IO => {
                    out.items.push(ResourceItem::FixedIo {
                        base: get_u16(body, 0)? & 0x3FF,
                        len: get(body, 2)?,
                    });
                }
                SMALL_VENDOR => {
                    out.items.push(ResourceItem::Vendor {
                        bytes: body.to_vec(),
                    });
                }
                _ => {
                    out.items.push(ResourceItem::Unknown { tag: head });
                }
            }
            pos = body_start + len;
        } else {
            // Large item: 16-bit length follows the tag byte.
            let ty = head & 0x7F;
            let len = get_u16(bytes, pos + 1)? as usize;
            let body_start = pos + 3;
            if body_start + len > bytes.len() {
                return Err(AmlError::Truncated);
            }
            let body = &bytes[body_start..body_start + len];
            match ty {
                LARGE_MEMORY32_FIXED => {
                    out.items.push(ResourceItem::Memory32Fixed {
                        writable: get(body, 0)? & 0x01 != 0,
                        base: get_u32(body, 1)?,
                        len: get_u32(body, 5)?,
                    });
                }
                LARGE_EXT_IRQ => {
                    let flags = get(body, 0)?;
                    let count = get(body, 1)? as usize;
                    let mut gsis = Vec::with_capacity(count);
                    for i in 0..count {
                        gsis.push(get_u32(body, 2 + 4 * i)?);
                    }
                    out.items.push(ResourceItem::Interrupt {
                        gsis,
                        consumer: flags & 0x01 != 0,
                        trigger: if flags & 0x02 != 0 {
                            Trigger::Edge
                        } else {
                            Trigger::Level
                        },
                        polarity: if flags & 0x04 != 0 {
                            Polarity::ActiveLow
                        } else {
                            Polarity::ActiveHigh
                        },
                        shared: flags & 0x08 != 0,
                        wake: flags & 0x10 != 0,
                    });
                }
                LARGE_ADDR16 => {
                    out.items.push(ResourceItem::Address {
                        resource_type: get(body, 0)?,
                        min: get_u16(body, 4)? as u64,
                        max: get_u16(body, 6)? as u64,
                        translation: get_u16(body, 8)? as u64,
                        len: get_u16(body, 10)? as u64,
                    });
                }
                LARGE_ADDR32 => {
                    out.items.push(ResourceItem::Address {
                        resource_type: get(body, 0)?,
                        min: get_u32(body, 6)? as u64,
                        max: get_u32(body, 10)? as u64,
                        translation: get_u32(body, 14)? as u64,
                        len: get_u32(body, 18)? as u64,
                    });
                }
                LARGE_ADDR64 | LARGE_EXT_ADDR64 => {
                    let off = if ty == LARGE_EXT_ADDR64 { 1 } else { 0 };
                    out.items.push(ResourceItem::Address {
                        resource_type: get(body, 0)?,
                        min: get_u64(body, 6 + off + 8)?,
                        max: get_u64(body, 6 + off + 16)?,
                        translation: get_u64(body, 6 + off + 24)?,
                        len: get_u64(body, 6 + off + 32)?,
                    });
                }
                LARGE_GPIO => {
                    // GPIO Connection (§6.4.3.8.1): fields at fixed
                    // offsets, then pin table + source string located by
                    // in-descriptor offsets (relative to the tag byte).
                    let conn_type = get(body, 1)?; // 0 = Interrupt, 1 = IO
                    let general_flags = get_u16(body, 2)?;
                    let int_flags = get_u16(body, 4)?;
                    let pin_config = get(body, 6)?;
                    let debounce = get_u16(body, 9)?;
                    let pin_off = get_u16(body, 11)? as usize;
                    let src_off = get_u16(body, 14)? as usize;
                    let vendor_off = get_u16(body, 17)? as usize;
                    // Offsets count from the descriptor head (tag byte);
                    // body starts 3 bytes in.
                    let rel = |o: usize| o.checked_sub(3).ok_or(AmlError::Truncated);
                    let pin_start = rel(pin_off)?;
                    let src_start = rel(src_off)?;
                    let pin_end = if src_off > pin_off {
                        src_start
                    } else {
                        body.len()
                    };
                    let pin_end = if vendor_off > pin_off && vendor_off <= body.len() + 3 {
                        pin_end.min(rel(vendor_off)?)
                    } else {
                        pin_end
                    };
                    if pin_start > body.len() || pin_end > body.len() || pin_start > pin_end {
                        return Err(AmlError::Truncated);
                    }
                    let mut pins = Vec::new();
                    let mut p = pin_start;
                    while p + 1 < pin_end {
                        pins.push(get_u16(body, p)?);
                        p += 2;
                    }
                    // Source: skip the ResourceSource index byte the
                    // string is preceded by in this descriptor.
                    let source = source_string(body, src_start);
                    if conn_type == 0 {
                        out.items.push(ResourceItem::GpioInt {
                            pins,
                            trigger: if int_flags & 0x01 != 0 {
                                Trigger::Edge
                            } else {
                                Trigger::Level
                            },
                            polarity: match (int_flags >> 1) & 0x03 {
                                0 => Polarity::ActiveHigh,
                                1 => Polarity::ActiveLow,
                                _ => Polarity::ActiveBoth,
                            },
                            shared: int_flags & 0x08 != 0,
                            wake: int_flags & 0x10 != 0,
                            pin_config,
                            debounce,
                            source,
                        });
                    } else {
                        out.items.push(ResourceItem::GpioIo {
                            pins,
                            shared: general_flags & 0x08 != 0,
                            pin_config,
                            debounce,
                            source,
                        });
                    }
                }
                LARGE_SERIAL_BUS => {
                    // Serial Bus Connection (§6.4.3.8.2). Common header:
                    //   0: revision id
                    //   1: resource source index
                    //   2: bus type (1 I2C, 2 SPI, 3 UART, 4 CSI-2)
                    //   3: general flags
                    //   4-5: type-specific flags
                    //   6: type-specific revision id
                    //   7-8: type data length (N)
                    //   9..9+N: type-specific data
                    //   9+N..: resource source string
                    let bus_type = get(body, 2)?;
                    let type_flags = get_u16(body, 4)?;
                    let type_len = get_u16(body, 7)? as usize;
                    let src_start = 9usize.checked_add(type_len).ok_or(AmlError::Truncated)?;
                    let source = source_string(body, src_start);
                    if bus_type == 1 {
                        // I2C: connection speed (u32) + slave address
                        // (u16) lead the type-specific data.
                        if type_len < 6 {
                            return Err(AmlError::Truncated);
                        }
                        out.items.push(ResourceItem::I2cSerialBus {
                            speed_hz: get_u32(body, 9)?,
                            address: get_u16(body, 13)?,
                            ten_bit: type_flags & 0x01 != 0,
                            slave: type_flags & 0x02 == 0,
                            source,
                        });
                    } else {
                        out.items
                            .push(ResourceItem::SerialBusOther { bus_type, source });
                    }
                }
                LARGE_VENDOR => {
                    out.items.push(ResourceItem::Vendor {
                        bytes: body.to_vec(),
                    });
                }
                _ => {
                    out.items.push(ResourceItem::Unknown { tag: head });
                }
            }
            pos = body_start + len;
        }
    }
    // No End Tag: return what decoded (real firmware occasionally omits
    // it on `_PRS` fragments).
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The touchpad-shaped fixture: I2C SerialBus (addr 0x2C @ 400 kHz on
    /// \_SB.PCI0.I2C1) + GpioInt (level, active-low, pin 0x112) — the
    /// same shape the Dell's DLL0945 `_CRS` yields.
    fn touchpad_crs() -> Vec<u8> {
        let mut v = Vec::new();
        // --- I2C SerialBus ---
        let src = b"\\_SB.PCI0.I2C1";
        let type_len = 6u16;
        let body_len = 9 + type_len as usize + src.len() + 1;
        v.push(0x8E);
        v.extend_from_slice(&(body_len as u16).to_le_bytes());
        v.push(2); // revision
        v.push(0); // source index
        v.push(1); // bus type = I2C
        v.push(0x02); // general flags (consumer)
        v.extend_from_slice(&0u16.to_le_bytes()); // type flags: 7-bit, slave
        v.push(1); // type revision
        v.extend_from_slice(&type_len.to_le_bytes());
        v.extend_from_slice(&400_000u32.to_le_bytes()); // speed
        v.extend_from_slice(&0x2Cu16.to_le_bytes()); // address
        v.extend_from_slice(src);
        v.push(0);
        // --- GpioInt ---
        let src2 = b"\\_SB.GPI0";
        // Layout (offsets relative to tag byte): header 23 bytes
        // (0x17), pin table at 0x17, source at 0x19.
        let pin_off = 23u16;
        let src_off = pin_off + 2;
        let vendor_off = src_off + src2.len() as u16 + 1;
        let body2_len = (vendor_off - 3) as usize;
        v.push(0x8C);
        v.extend_from_slice(&(body2_len as u16).to_le_bytes());
        let gpio_start = v.len();
        v.push(1); // revision
        v.push(0); // connection type = Interrupt
        v.extend_from_slice(&0u16.to_le_bytes()); // general flags
        // int flags: level (bit0=0), active-low (bits2:1 = 01), shared=0
        v.extend_from_slice(&0x0002u16.to_le_bytes());
        v.push(1); // pin config = pull-up
        v.extend_from_slice(&0u16.to_le_bytes()); // drive strength
        v.extend_from_slice(&0u16.to_le_bytes()); // debounce
        v.extend_from_slice(&pin_off.to_le_bytes());
        v.push(0); // source index
        v.extend_from_slice(&src_off.to_le_bytes());
        v.extend_from_slice(&vendor_off.to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes()); // vendor length
        v.extend_from_slice(&0x0112u16.to_le_bytes()); // pin 274
        v.extend_from_slice(src2);
        v.push(0);
        assert_eq!(v.len() - gpio_start, body2_len);
        // --- End tag ---
        v.push(0x79);
        v.push(0x00);
        v
    }

    #[test]
    fn touchpad_crs_decodes() {
        let r = decode_crs(&touchpad_crs()).unwrap();
        let Some(ResourceItem::I2cSerialBus {
            address,
            speed_hz,
            ten_bit,
            slave,
            source,
        }) = r.i2c()
        else {
            panic!("no I2C item: {:?}", r.items);
        };
        assert_eq!(*address, 0x2C);
        assert_eq!(*speed_hz, 400_000);
        assert!(!ten_bit);
        assert!(slave);
        assert_eq!(source, "\\_SB.PCI0.I2C1");

        let Some(ResourceItem::GpioInt {
            pins,
            trigger,
            polarity,
            pin_config,
            source,
            ..
        }) = r.gpio_int()
        else {
            panic!("no GpioInt item: {:?}", r.items);
        };
        assert_eq!(pins.as_slice(), &[0x0112]);
        assert_eq!(*trigger, Trigger::Level);
        assert_eq!(*polarity, Polarity::ActiveLow);
        assert_eq!(*pin_config, 1);
        assert_eq!(source, "\\_SB.GPI0");
    }

    #[test]
    fn classic_descriptors_decode() {
        // IO 0x3F8..0x3F8 len 8, IRQ 4 (edge/high), Memory32Fixed, EndTag —
        // the COM1-style set QEMU's DSDT carries.
        let bytes: &[u8] = &[
            0x47, 0x01, 0xF8, 0x03, 0xF8, 0x03, 0x01, 0x08, // IO
            0x22, 0x10, 0x00, // IRQ mask: bit 4, no flags byte
            0x86, 0x09, 0x00, 0x01, 0x00, 0x00, 0x0C, 0xFE, 0x00, 0x10, 0x00,
            0x00, // Mem32Fixed
            0x79, 0x00, // EndTag
            0xAA, 0xBB, // trailing garbage: ignored
        ];
        let r = decode_crs(bytes).unwrap();
        assert_eq!(r.items.len(), 3);
        assert_eq!(
            r.items[0],
            ResourceItem::Io {
                decode16: true,
                min: 0x3F8,
                max: 0x3F8,
                align: 1,
                len: 8
            }
        );
        assert_eq!(r.interrupts(), alloc::vec![4]);
        let ResourceItem::Memory32Fixed {
            base,
            len,
            writable,
        } = r.items[2]
        else {
            panic!("not mem32: {:?}", r.items[2]);
        };
        assert!(writable);
        assert_eq!(base, 0xFE0C_0000);
        assert_eq!(len, 0x1000);
    }

    #[test]
    fn extended_interrupt_decodes() {
        let bytes: &[u8] = &[
            0x89, 0x06, 0x00, // ExtIrq, len 6
            0x0F, // consumer, edge, active-low, shared
            0x01, // one GSI
            0x2A, 0x00, 0x00, 0x00, // GSI 42
            0x79, 0x00,
        ];
        let r = decode_crs(bytes).unwrap();
        let ResourceItem::Interrupt {
            ref gsis,
            trigger,
            polarity,
            shared,
            wake,
            consumer,
        } = r.items[0]
        else {
            panic!("not interrupt");
        };
        assert_eq!(gsis.as_slice(), &[42]);
        assert_eq!(trigger, Trigger::Edge);
        assert_eq!(polarity, Polarity::ActiveLow);
        assert!(shared && consumer && !wake);
    }

    #[test]
    fn truncated_and_malformed_are_errors_not_panics() {
        let good = touchpad_crs();
        for cut in 0..good.len() {
            let _ = decode_crs(&good[..cut]);
        }
        // Large item length pointing past the end.
        assert_eq!(
            decode_crs(&[0x8E, 0xFF, 0x00, 0x01]),
            Err(AmlError::Truncated)
        );
        // Empty input decodes to nothing.
        assert_eq!(decode_crs(&[]).unwrap().items.len(), 0);
    }
}
