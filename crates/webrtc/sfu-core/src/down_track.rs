use std::collections::VecDeque;
use std::time::Duration;

use fluvora_media_codec::PayloadInfo;
use fluvora_rtp::{Packet, Rewrite, rewrite_header_extensions};

use crate::{
    ForwardedPacket, Layer, MediaKind, ParticipantId, PublishedTrack, SfuError, SfuEvent,
    SubscriptionConfig,
};

#[derive(Debug, Clone)]
struct CachedPacket {
    inserted_at: Duration,
    packet: ForwardedPacket,
}

#[derive(Debug, Clone)]
pub(crate) struct DownTrack {
    pub configuration: SubscriptionConfig,
    target_layer: Layer,
    selected_spatial: Option<u8>,
    current_input_ssrc: Option<u32>,
    input_timestamp_base: u32,
    output_timestamp_base: u32,
    last_output_timestamp: Option<u32>,
    timestamp_step: u32,
    next_sequence_number: u16,
    cache: VecDeque<CachedPacket>,
    cache_capacity: usize,
    cache_age: Duration,
}

pub(crate) enum ForwardDecision {
    Forwarded {
        packet: ForwardedPacket,
        event: Option<SfuEvent>,
    },
    Dropped {
        waiting_for_keyframe: bool,
    },
}

impl DownTrack {
    pub fn new(
        configuration: SubscriptionConfig,
        track: &PublishedTrack,
        cache_capacity: usize,
        cache_age: Duration,
    ) -> Self {
        let timestamp_step = match track.kind {
            MediaKind::Audio => track.clock_rate / 50,
            MediaKind::Video => track.clock_rate / 30,
        }
        .max(1);
        Self {
            target_layer: configuration.initial_layer,
            selected_spatial: None,
            current_input_ssrc: None,
            input_timestamp_base: 0,
            output_timestamp_base: configuration.initial_timestamp,
            last_output_timestamp: None,
            timestamp_step,
            next_sequence_number: configuration.initial_sequence_number,
            configuration,
            cache: VecDeque::new(),
            cache_capacity,
            cache_age,
        }
    }

    pub const fn subscriber(&self) -> ParticipantId {
        self.configuration.subscriber
    }

    pub const fn track_id(&self) -> crate::TrackId {
        self.configuration.track_id
    }

    pub const fn output_ssrc(&self) -> u32 {
        self.configuration.output_ssrc
    }

    pub const fn target_layer(&self) -> Layer {
        self.target_layer
    }

    pub const fn selected_spatial(&self) -> Option<u8> {
        self.selected_spatial
    }

    pub fn set_target_layer(&mut self, layer: Layer) -> bool {
        if self.target_layer == layer {
            false
        } else {
            self.target_layer = layer;
            true
        }
    }

    pub fn forward(
        &mut self,
        now: Duration,
        track: &PublishedTrack,
        spatial_layer: u8,
        info: PayloadInfo,
        input: &[u8],
    ) -> Result<ForwardDecision, SfuError> {
        let selected = self.selected_spatial;
        let switching_to_target =
            selected != Some(spatial_layer) && spatial_layer == self.target_layer.spatial;
        let continuing_selected = selected == Some(spatial_layer);
        if !continuing_selected && !switching_to_target {
            return Ok(ForwardDecision::Dropped {
                waiting_for_keyframe: selected != Some(self.target_layer.spatial),
            });
        }
        let requires_keyframe = track.kind == MediaKind::Video && !continuing_selected;
        if requires_keyframe && !(info.keyframe && info.start_of_frame) {
            return Ok(ForwardDecision::Dropped {
                waiting_for_keyframe: true,
            });
        }
        if info
            .temporal_id
            .is_some_and(|temporal| temporal > self.target_layer.temporal)
        {
            return Ok(ForwardDecision::Dropped {
                waiting_for_keyframe: false,
            });
        }

        let parsed = Packet::parse(input)?;
        let switched = self.current_input_ssrc != Some(parsed.header().ssrc);
        let previous_spatial = self.selected_spatial;
        let output_timestamp = self.rewrite_timestamp(parsed.header().timestamp, switched);
        let output_sequence = self.next_sequence_number;
        self.next_sequence_number = self.next_sequence_number.wrapping_add(1);
        if switched {
            self.current_input_ssrc = Some(parsed.header().ssrc);
            self.selected_spatial = Some(spatial_layer);
        }
        let mut packet = input.to_vec();
        Rewrite {
            marker: None,
            payload_type: Some(self.configuration.output_payload_type),
            sequence_number: Some(output_sequence),
            timestamp: Some(output_timestamp),
            ssrc: Some(self.configuration.output_ssrc),
        }
        .apply(&mut packet)?;
        packet = rewrite_header_extensions(&packet, &self.configuration.extension_rewrites)?;
        let forwarded = ForwardedPacket {
            subscriber: self.configuration.subscriber,
            subscription_id: self.configuration.id,
            packet,
            layer: Layer {
                spatial: spatial_layer,
                temporal: info.temporal_id.unwrap_or(0),
            },
            keyframe: info.keyframe,
            retransmission: false,
        };
        self.cache(now, forwarded.clone());
        let event = switched.then_some(SfuEvent::LayerSwitched {
            subscription_id: self.configuration.id,
            from: previous_spatial,
            to: spatial_layer,
        });
        Ok(ForwardDecision::Forwarded {
            packet: forwarded,
            event,
        })
    }

    pub fn retransmit(&mut self, now: Duration, sequence_numbers: &[u16]) -> Vec<ForwardedPacket> {
        self.expire_cache(now);
        sequence_numbers
            .iter()
            .filter_map(|requested| {
                self.cache.iter().rev().find_map(|cached| {
                    let packet = Packet::parse(&cached.packet.packet).ok()?;
                    (packet.header().sequence_number == *requested).then(|| {
                        let mut retransmission = cached.packet.clone();
                        retransmission.retransmission = true;
                        retransmission
                    })
                })
            })
            .collect()
    }

    fn rewrite_timestamp(&mut self, input_timestamp: u32, switched: bool) -> u32 {
        let output = if self.current_input_ssrc.is_none() {
            self.input_timestamp_base = input_timestamp;
            self.output_timestamp_base
        } else if switched {
            self.input_timestamp_base = input_timestamp;
            self.output_timestamp_base = self
                .last_output_timestamp
                .unwrap_or(self.output_timestamp_base)
                .wrapping_add(self.timestamp_step);
            self.output_timestamp_base
        } else {
            self.output_timestamp_base
                .wrapping_add(input_timestamp.wrapping_sub(self.input_timestamp_base))
        };
        if let Some(previous) = self.last_output_timestamp {
            let step = output.wrapping_sub(previous);
            if step > 0 && step < 900_000 {
                self.timestamp_step = step;
            }
        }
        self.last_output_timestamp = Some(output);
        output
    }

    fn cache(&mut self, now: Duration, packet: ForwardedPacket) {
        self.expire_cache(now);
        if self.cache_capacity == 0 {
            return;
        }
        while self.cache.len() >= self.cache_capacity {
            self.cache.pop_front();
        }
        self.cache.push_back(CachedPacket {
            inserted_at: now,
            packet,
        });
    }

    fn expire_cache(&mut self, now: Duration) {
        while self
            .cache
            .front()
            .is_some_and(|packet| now.saturating_sub(packet.inserted_at) > self.cache_age)
        {
            self.cache.pop_front();
        }
    }
}
