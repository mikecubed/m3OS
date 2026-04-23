pub mod events;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScancodeSink {
    Tty,
    Raw,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScancodeRouter {
    latched_sink: Option<ScancodeSink>,
    remaining_prefixed_bytes: u8,
}

impl ScancodeRouter {
    pub const fn new() -> Self {
        Self {
            latched_sink: None,
            remaining_prefixed_bytes: 0,
        }
    }

    pub fn route_byte(&mut self, byte: u8, raw_owner_active: bool) -> ScancodeSink {
        if let Some(sink) = self.latched_sink {
            self.remaining_prefixed_bytes = self.remaining_prefixed_bytes.saturating_sub(1);
            if self.remaining_prefixed_bytes == 0 {
                self.latched_sink = None;
            }
            return sink;
        }

        let sink = if raw_owner_active {
            ScancodeSink::Raw
        } else {
            ScancodeSink::Tty
        };

        self.remaining_prefixed_bytes = match byte {
            0xE0 => 1,
            0xE1 => 5,
            _ => 0,
        };
        if self.remaining_prefixed_bytes != 0 {
            self.latched_sink = Some(sink);
        }

        sink
    }

    pub fn reset(&mut self) {
        self.latched_sink = None;
        self.remaining_prefixed_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::{ScancodeRouter, ScancodeSink};

    #[test]
    fn normal_byte_tracks_current_owner() {
        let mut router = ScancodeRouter::new();

        assert_eq!(router.route_byte(0x1e, false), ScancodeSink::Tty);
        assert_eq!(router.route_byte(0x1e, true), ScancodeSink::Raw);
    }

    #[test]
    fn extended_sequence_stays_on_original_sink_until_complete() {
        let mut router = ScancodeRouter::new();

        assert_eq!(router.route_byte(0xe0, true), ScancodeSink::Raw);
        assert_eq!(router.route_byte(0x48, false), ScancodeSink::Raw);
        assert_eq!(router.route_byte(0x1e, false), ScancodeSink::Tty);
    }

    #[test]
    fn pause_sequence_stays_on_original_sink_for_all_bytes() {
        let mut router = ScancodeRouter::new();

        assert_eq!(router.route_byte(0xe1, false), ScancodeSink::Tty);
        for byte in [0x1d, 0x45, 0xe1, 0x9d, 0xc5] {
            assert_eq!(router.route_byte(byte, true), ScancodeSink::Tty);
        }
        assert_eq!(router.route_byte(0x1e, true), ScancodeSink::Raw);
    }

    #[test]
    fn reset_drops_pending_sequence() {
        let mut router = ScancodeRouter::new();

        assert_eq!(router.route_byte(0xe0, true), ScancodeSink::Raw);
        router.reset();
        assert_eq!(router.route_byte(0x48, false), ScancodeSink::Tty);
    }
}
