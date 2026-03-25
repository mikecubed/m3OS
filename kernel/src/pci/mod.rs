//! PCI bus enumeration via Configuration Space mechanism #1 (I/O ports 0xCF8/0xCFC).
//!
//! Scans all 256 buses, 32 devices per bus, up to 8 functions per device,
//! and stores discovered devices in a static list for later use.

use spin::Mutex;
use x86_64::instructions::{interrupts, port::Port};

// ---------------------------------------------------------------------------
// PCI Configuration Space I/O (P15-T033, P15-T034)
// ---------------------------------------------------------------------------

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

/// Build the 32-bit address for PCI configuration space access.
fn config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    (1u32 << 31)
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

/// Read a 32-bit value from PCI configuration space.
///
/// Interrupts are disabled for the duration of the two-port transaction
/// to prevent races on the shared CONFIG_ADDRESS/CONFIG_DATA pair.
fn pci_config_read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let addr = config_address(bus, device, function, offset);
    interrupts::without_interrupts(|| {
        // SAFETY: Ports 0xCF8 and 0xCFC are the well-defined PCI configuration
        // space I/O ports on x86. Writing an address and reading data is the
        // standard mechanism #1 access pattern.
        unsafe {
            let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
            let mut data_port = Port::<u32>::new(CONFIG_DATA);
            addr_port.write(addr);
            data_port.read()
        }
    })
}

/// Read a 16-bit value from PCI configuration space.
pub fn pci_config_read_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let dword = pci_config_read_u32(bus, device, function, offset);
    // The offset's low bit selects which 16-bit half of the 32-bit value.
    let shift = ((offset & 2) as u32) * 8;
    ((dword >> shift) & 0xFFFF) as u16
}

/// Read an 8-bit value from PCI configuration space.
#[allow(dead_code)]
pub fn pci_config_read_u8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let dword = pci_config_read_u32(bus, device, function, offset);
    let shift = ((offset & 3) as u32) * 8;
    ((dword >> shift) & 0xFF) as u8
}

// ---------------------------------------------------------------------------
// PciDevice (P15-T035)
// ---------------------------------------------------------------------------

/// Describes a single PCI function discovered during bus enumeration.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub header_type: u8,
    pub bars: [u32; 6],
    pub interrupt_line: u8,
    pub interrupt_pin: u8,
}

// ---------------------------------------------------------------------------
// Static storage (P15-T038)
// ---------------------------------------------------------------------------

const MAX_PCI_DEVICES: usize = 64;

struct PciDeviceList {
    devices: [Option<PciDevice>; MAX_PCI_DEVICES],
    count: usize,
}

impl PciDeviceList {
    const fn new() -> Self {
        Self {
            devices: [None; MAX_PCI_DEVICES],
            count: 0,
        }
    }

    fn push(&mut self, dev: PciDevice) -> bool {
        if self.count < MAX_PCI_DEVICES {
            self.devices[self.count] = Some(dev);
            self.count += 1;
            true
        } else {
            false
        }
    }
}

static PCI_DEVICES: Mutex<PciDeviceList> = Mutex::new(PciDeviceList::new());

// ---------------------------------------------------------------------------
// Read-only accessors (P15-T039)
// ---------------------------------------------------------------------------

/// Returns the number of PCI devices discovered during the last scan.
#[allow(dead_code)]
pub fn pci_device_count() -> usize {
    PCI_DEVICES.lock().count
}

/// Returns a copy of the PCI device at the given index, or `None`.
pub fn pci_device(index: usize) -> Option<PciDevice> {
    let list = PCI_DEVICES.lock();
    if index < list.count {
        list.devices[index]
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Device probing (P15-T037)
// ---------------------------------------------------------------------------

/// Read all relevant fields for a single PCI function and return a `PciDevice`.
fn probe_function(bus: u8, device: u8, function: u8) -> PciDevice {
    // Offset 0x00: vendor_id (low 16), device_id (high 16)
    let id_reg = pci_config_read_u32(bus, device, function, 0x00);
    let vendor_id = id_reg as u16;
    let device_id = (id_reg >> 16) as u16;

    // Offset 0x08: revision (byte 0), prog_if (byte 1), subclass (byte 2), class (byte 3)
    let class_reg = pci_config_read_u32(bus, device, function, 0x08);
    let prog_if = ((class_reg >> 8) & 0xFF) as u8;
    let subclass = ((class_reg >> 16) & 0xFF) as u8;
    let class_code = ((class_reg >> 24) & 0xFF) as u8;

    // Offset 0x0C: header_type is byte 2
    let hdr_reg = pci_config_read_u32(bus, device, function, 0x0C);
    let header_type = ((hdr_reg >> 16) & 0xFF) as u8;

    // BARs: only for header type 0 (general device). Header type 1 (PCI-PCI bridge)
    // and type 2 (CardBus) have different layouts.
    let mut bars = [0u32; 6];
    if header_type & 0x7F == 0x00 {
        for (i, bar) in bars.iter_mut().enumerate() {
            *bar = pci_config_read_u32(bus, device, function, 0x10 + (i as u8) * 4);
        }
    }

    // Offset 0x3C: interrupt_line (byte 0), interrupt_pin (byte 1)
    let int_reg = pci_config_read_u32(bus, device, function, 0x3C);
    let interrupt_line = (int_reg & 0xFF) as u8;
    let interrupt_pin = ((int_reg >> 8) & 0xFF) as u8;

    PciDevice {
        bus,
        device,
        function,
        vendor_id,
        device_id,
        class_code,
        subclass,
        prog_if,
        header_type,
        bars,
        interrupt_line,
        interrupt_pin,
    }
}

// ---------------------------------------------------------------------------
// Bus scan (P15-T036)
// ---------------------------------------------------------------------------

/// Scan all PCI buses and populate the global device list.
fn pci_scan() {
    let mut list = PCI_DEVICES.lock();
    list.count = 0;
    for i in 0..MAX_PCI_DEVICES {
        list.devices[i] = None;
    }

    for bus in 0..=255u16 {
        let bus = bus as u8;
        for device in 0..32u8 {
            // Check if function 0 exists.
            let vendor = pci_config_read_u16(bus, device, 0, 0x00);
            if vendor == 0xFFFF {
                continue;
            }

            // Probe function 0.
            let dev0 = probe_function(bus, device, 0);
            if !list.push(dev0) {
                log::warn!("[pci] device list full, stopping scan");
                return;
            }

            // Check multi-function bit (bit 7 of header_type at function 0).
            let multi_function = dev0.header_type & 0x80 != 0;
            if !multi_function {
                continue;
            }

            // Scan remaining functions 1..7.
            for function in 1..8u8 {
                let vendor = pci_config_read_u16(bus, device, function, 0x00);
                if vendor == 0xFFFF {
                    continue;
                }
                let dev = probe_function(bus, device, function);
                if !list.push(dev) {
                    log::warn!("[pci] device list full, stopping scan");
                    return;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Logging (P15-T040)
// ---------------------------------------------------------------------------

/// Return a human-readable description for common PCI class/subclass pairs.
fn class_description(class: u8, subclass: u8) -> &'static str {
    match (class, subclass) {
        (0x00, 0x00) => "Unclassified",
        (0x00, _) => "Unclassified",
        (0x01, 0x00) => "SCSI Bus Controller",
        (0x01, 0x01) => "IDE Controller",
        (0x01, 0x06) => "SATA Controller",
        (0x01, _) => "Mass Storage",
        (0x02, 0x00) => "Ethernet Controller",
        (0x02, _) => "Network",
        (0x03, 0x00) => "VGA Controller",
        (0x03, _) => "Display",
        (0x04, _) => "Multimedia",
        (0x05, _) => "Memory Controller",
        (0x06, 0x00) => "Host Bridge",
        (0x06, 0x01) => "ISA Bridge",
        (0x06, 0x04) => "PCI-to-PCI Bridge",
        (0x06, 0x80) => "Other Bridge",
        (0x06, _) => "Bridge",
        (0x07, _) => "Communication Controller",
        (0x08, _) => "System Peripheral",
        (0x0C, 0x03) => "USB Controller",
        (0x0C, _) => "Serial Bus Controller",
        _ => "Unknown",
    }
}

/// Scan PCI buses and log all discovered devices.
pub fn pci_scan_and_log() {
    pci_scan();

    let count = {
        let list = PCI_DEVICES.lock();
        list.count
    };

    log::info!("[pci] discovered {} device(s)", count);

    for i in 0..count {
        if let Some(dev) = pci_device(i) {
            log::info!(
                "[pci] {:02x}:{:02x}.{} {:04x}:{:04x} {:02x}/{:02x} ({})",
                dev.bus,
                dev.device,
                dev.function,
                dev.vendor_id,
                dev.device_id,
                dev.class_code,
                dev.subclass,
                class_description(dev.class_code, dev.subclass),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Module init (called from kernel_main)
// ---------------------------------------------------------------------------

/// Initialize PCI subsystem: enumerate buses and log devices.
pub fn init() {
    pci_scan_and_log();
}
