use std::time::Duration;

use fluvora_sfu_core::Layer;

/// One encoding/layer combination and its sustainable bitrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerOption {
    /// SFU spatial and temporal target.
    pub layer: Layer,
    /// Minimum target bitrate required to select it.
    pub minimum_bitrate_bps: u64,
}

/// Stateful layer allocator with immediate downgrade and delayed upgrade.
#[derive(Debug, Clone)]
pub struct LayerSelector {
    current: Option<Layer>,
    upgrade_candidate: Option<(Layer, Duration)>,
    upgrade_hold: Duration,
    upgrade_headroom_percent: u64,
}

impl Default for LayerSelector {
    fn default() -> Self {
        Self {
            current: None,
            upgrade_candidate: None,
            upgrade_hold: Duration::from_secs(2),
            upgrade_headroom_percent: 120,
        }
    }
}

impl LayerSelector {
    /// Selects a sustainable layer from options sorted internally by bitrate.
    ///
    /// Downgrades happen immediately. Upgrades require 20% headroom for two seconds.
    #[must_use]
    pub fn select(
        &mut self,
        now: Duration,
        options: &[LayerOption],
        target_bitrate_bps: u64,
    ) -> Option<Layer> {
        let mut sorted = options.to_vec();
        sorted.sort_unstable_by_key(|option| option.minimum_bitrate_bps);
        let affordable = sorted
            .iter()
            .rev()
            .find(|option| option.minimum_bitrate_bps <= target_bitrate_bps)
            .copied()
            .or_else(|| sorted.first().copied())?;
        let Some(current) = self.current else {
            self.current = Some(affordable.layer);
            return self.current;
        };
        let current_rate = layer_rate(&sorted, current).unwrap_or(u64::MAX);
        let affordable_rate = affordable.minimum_bitrate_bps;
        if affordable_rate < current_rate {
            self.current = Some(affordable.layer);
            self.upgrade_candidate = None;
            return self.current;
        }
        if affordable_rate == current_rate {
            self.upgrade_candidate = None;
            return self.current;
        }
        let required = affordable_rate.saturating_mul(self.upgrade_headroom_percent) / 100;
        if target_bitrate_bps < required {
            self.upgrade_candidate = None;
            return self.current;
        }
        match self.upgrade_candidate {
            Some((layer, since))
                if layer == affordable.layer && now.saturating_sub(since) >= self.upgrade_hold =>
            {
                self.current = Some(affordable.layer);
                self.upgrade_candidate = None;
            }
            Some((layer, _)) if layer == affordable.layer => {}
            _ => self.upgrade_candidate = Some((affordable.layer, now)),
        }
        self.current
    }

    /// Returns the committed layer.
    #[must_use]
    pub const fn current(&self) -> Option<Layer> {
        self.current
    }
}

fn layer_rate(options: &[LayerOption], layer: Layer) -> Option<u64> {
    options
        .iter()
        .find(|option| option.layer == layer)
        .map(|option| option.minimum_bitrate_bps)
}
