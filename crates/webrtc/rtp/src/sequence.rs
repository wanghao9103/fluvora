/// Extends wrapping 16-bit RTP sequence numbers into a monotonic 64-bit space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SequenceNumberExtender {
    highest: Option<u64>,
}

impl SequenceNumberExtender {
    /// Creates an empty sequence-number history.
    #[must_use]
    pub const fn new() -> Self {
        Self { highest: None }
    }

    /// Maps a sequence number to the nearest cycle and advances the high-water mark for new data.
    #[must_use]
    pub fn extend(&mut self, value: u16) -> u64 {
        let extended = self.highest.map_or(u64::from(value), |highest| {
            extend_wrapping(highest, u64::from(value), 1 << 16)
        });
        if self.highest.is_none_or(|highest| extended > highest) {
            self.highest = Some(extended);
        }
        extended
    }

    /// Returns the greatest extended value observed.
    #[must_use]
    pub const fn highest(&self) -> Option<u64> {
        self.highest
    }
}

/// Extends wrapping 32-bit RTP timestamps into a monotonic 64-bit space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimestampExtender {
    highest: Option<u64>,
}

impl TimestampExtender {
    /// Creates an empty timestamp history.
    #[must_use]
    pub const fn new() -> Self {
        Self { highest: None }
    }

    /// Maps a timestamp to the nearest cycle and advances the high-water mark for new data.
    #[must_use]
    pub fn extend(&mut self, value: u32) -> u64 {
        let extended = self.highest.map_or(u64::from(value), |highest| {
            extend_wrapping(highest, u64::from(value), 1_u64 << 32)
        });
        if self.highest.is_none_or(|highest| extended > highest) {
            self.highest = Some(extended);
        }
        extended
    }

    /// Returns the greatest extended value observed.
    #[must_use]
    pub const fn highest(&self) -> Option<u64> {
        self.highest
    }
}

fn extend_wrapping(highest: u64, value: u64, modulus: u64) -> u64 {
    let half = modulus / 2;
    let base = highest - (highest % modulus);
    let mut candidate = base + value;
    if candidate.saturating_add(half) < highest {
        candidate = candidate.saturating_add(modulus);
    } else if candidate > highest.saturating_add(half) && candidate >= modulus {
        candidate -= modulus;
    }
    candidate
}
