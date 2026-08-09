use crate::SrtpError;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ReplayWindow {
    maximum: Option<u64>,
    bitmap: u64,
}

impl ReplayWindow {
    pub const fn maximum(self) -> Option<u64> {
        self.maximum
    }

    pub fn check(self, index: u64) -> Result<(), SrtpError> {
        let Some(maximum) = self.maximum else {
            return Ok(());
        };
        if index > maximum {
            return Ok(());
        }
        let distance = maximum - index;
        if distance >= 64 {
            return Err(SrtpError::PacketTooOld);
        }
        if self.bitmap & (1_u64 << distance) != 0 {
            Err(SrtpError::ReplayDetected)
        } else {
            Ok(())
        }
    }

    pub fn accept(&mut self, index: u64) {
        match self.maximum {
            None => {
                self.maximum = Some(index);
                self.bitmap = 1;
            }
            Some(maximum) if index > maximum => {
                let advance = index - maximum;
                self.bitmap = if advance >= 64 {
                    1
                } else {
                    (self.bitmap << advance) | 1
                };
                self.maximum = Some(index);
            }
            Some(maximum) => {
                self.bitmap |= 1_u64 << (maximum - index);
            }
        }
    }
}

pub(crate) fn estimate_index(highest: Option<u64>, sequence_number: u16) -> u64 {
    let Some(highest) = highest else {
        return u64::from(sequence_number);
    };
    let modulus = 1_u64 << 16;
    let half = modulus / 2;
    let base = highest - highest % modulus;
    let mut candidate = base + u64::from(sequence_number);
    if candidate.saturating_add(half) < highest {
        candidate = candidate.saturating_add(modulus);
    } else if candidate > highest.saturating_add(half) && candidate >= modulus {
        candidate -= modulus;
    }
    candidate
}
