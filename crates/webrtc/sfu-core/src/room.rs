use std::collections::{HashMap, HashSet};
use std::time::Duration;

use fluvora_media_codec::inspect_payload;
use fluvora_rtcp::{Packet as RtcpPacket, parse_compound};
use fluvora_rtp::Packet;

use crate::down_track::{DownTrack, ForwardDecision};
use crate::{
    ControlOutput, ForwardOutcome, Layer, MediaKind, ParticipantId, PublishedTrack, RoomConfig,
    SfuError, SfuEvent, SubscriptionConfig, SubscriptionId, TrackId,
};

/// All published tracks and subscriptions for one isolated SFU room.
#[derive(Debug)]
pub struct Room {
    configuration: RoomConfig,
    tracks: HashMap<TrackId, PublishedTrack>,
    ssrc_index: HashMap<u32, (TrackId, u8)>,
    subscriptions: HashMap<SubscriptionId, DownTrack>,
    track_subscriptions: HashMap<TrackId, Vec<SubscriptionId>>,
    output_ssrc_index: HashMap<u32, SubscriptionId>,
    last_pli: HashMap<u32, Duration>,
}

impl Room {
    /// Creates an empty bounded room.
    #[must_use]
    pub fn new(configuration: RoomConfig) -> Self {
        Self {
            configuration,
            tracks: HashMap::new(),
            ssrc_index: HashMap::new(),
            subscriptions: HashMap::new(),
            track_subscriptions: HashMap::new(),
            output_ssrc_index: HashMap::new(),
            last_pli: HashMap::new(),
        }
    }

    /// Registers a publisher track and its SSRC-to-layer mapping.
    ///
    /// # Errors
    ///
    /// Returns [`SfuError`] for duplicate identifiers, invalid encodings, unsafe payload types, or
    /// a reached room limit.
    pub fn publish(&mut self, track: PublishedTrack) -> Result<(), SfuError> {
        validate_track(&track)?;
        if self.tracks.contains_key(&track.id) {
            return Err(SfuError::DuplicateTrack(track.id));
        }
        if self.tracks.len() >= self.configuration.max_tracks {
            return Err(SfuError::ResourceLimit("published track"));
        }
        if track
            .encodings
            .iter()
            .any(|encoding| self.ssrc_index.contains_key(&encoding.ssrc))
        {
            return Err(SfuError::InvalidEncodings);
        }
        for encoding in &track.encodings {
            self.ssrc_index
                .insert(encoding.ssrc, (track.id, encoding.spatial_layer));
        }
        self.tracks.insert(track.id, track);
        Ok(())
    }

    /// Removes a track and all subscriptions that consume it.
    pub fn unpublish(&mut self, track_id: TrackId) -> bool {
        let Some(track) = self.tracks.remove(&track_id) else {
            return false;
        };
        for encoding in track.encodings {
            self.ssrc_index.remove(&encoding.ssrc);
            self.last_pli.remove(&encoding.ssrc);
        }
        if let Some(subscriptions) = self.track_subscriptions.remove(&track_id) {
            for id in subscriptions {
                self.remove_subscription(id);
            }
        }
        true
    }

    /// Returns immutable negotiated metadata for a published track.
    #[must_use]
    pub fn published_track(&self, track_id: TrackId) -> Option<&PublishedTrack> {
        self.tracks.get(&track_id)
    }

    /// Creates one subscriber down-track.
    ///
    /// # Errors
    ///
    /// Returns [`SfuError`] for missing tracks/layers, identifier collisions, unsafe payload types,
    /// or resource exhaustion.
    pub fn subscribe(&mut self, configuration: SubscriptionConfig) -> Result<(), SfuError> {
        if self.subscriptions.contains_key(&configuration.id) {
            return Err(SfuError::DuplicateSubscription(configuration.id));
        }
        if self
            .output_ssrc_index
            .contains_key(&configuration.output_ssrc)
        {
            return Err(SfuError::InvalidEncodings);
        }
        if !fluvora_rtc_datagram::is_rtcp_mux_safe_payload_type(configuration.output_payload_type) {
            return Err(SfuError::InvalidPayloadType(
                configuration.output_payload_type,
            ));
        }
        if self.subscriptions.len() >= self.configuration.max_subscriptions {
            return Err(SfuError::ResourceLimit("subscription"));
        }
        let track = self
            .tracks
            .get(&configuration.track_id)
            .ok_or(SfuError::UnknownTrack(configuration.track_id))?;
        if track
            .encoding(configuration.initial_layer.spatial)
            .is_none()
        {
            return Err(SfuError::UnknownSpatialLayer(
                configuration.initial_layer.spatial,
            ));
        }
        let id = configuration.id;
        let output_ssrc = configuration.output_ssrc;
        let track_id = configuration.track_id;
        let down_track = DownTrack::new(
            configuration,
            track,
            self.configuration.retransmission_cache_packets,
            self.configuration.retransmission_cache_age,
        );
        self.output_ssrc_index.insert(output_ssrc, id);
        self.track_subscriptions
            .entry(track_id)
            .or_default()
            .push(id);
        self.subscriptions.insert(id, down_track);
        Ok(())
    }

    /// Removes one subscription and its packet cache.
    pub fn unsubscribe(&mut self, id: SubscriptionId) -> bool {
        self.remove_subscription(id)
    }

    /// Requests an adaptive target. Video switches commit only on a target-encoding keyframe.
    ///
    /// # Errors
    ///
    /// Returns [`SfuError`] for a missing subscription, wrong subscriber, or unavailable layer.
    pub fn set_target_layer(
        &mut self,
        now: Duration,
        subscriber: ParticipantId,
        id: SubscriptionId,
        layer: Layer,
    ) -> Result<Vec<SfuEvent>, SfuError> {
        let (track_id, changed) = {
            let down_track = self
                .subscriptions
                .get_mut(&id)
                .ok_or(SfuError::UnknownSubscription(id))?;
            if down_track.subscriber() != subscriber {
                return Err(SfuError::UnauthorizedParticipant(subscriber));
            }
            (down_track.track_id(), down_track.set_target_layer(layer))
        };
        let track = self
            .tracks
            .get(&track_id)
            .ok_or(SfuError::UnknownTrack(track_id))?;
        let encoding = track
            .encoding(layer.spatial)
            .ok_or(SfuError::UnknownSpatialLayer(layer.spatial))?;
        if changed && track.kind == MediaKind::Video {
            Ok(self
                .maybe_pli(now, track_id, encoding.ssrc)
                .into_iter()
                .collect())
        } else {
            Ok(Vec::new())
        }
    }

    /// Routes one authenticated publisher RTP packet to eligible down-tracks.
    ///
    /// # Errors
    ///
    /// Returns [`SfuError`] for malformed RTP/payload descriptors, unknown SSRCs, or publisher
    /// authorization failure.
    pub fn handle_rtp(
        &mut self,
        now: Duration,
        publisher: ParticipantId,
        input: &[u8],
    ) -> Result<ForwardOutcome, SfuError> {
        let parsed = Packet::parse(input)?;
        let (track_id, spatial_layer) = self
            .ssrc_index
            .get(&parsed.header().ssrc)
            .copied()
            .ok_or(SfuError::UnknownSsrc(parsed.header().ssrc))?;
        let track = self
            .tracks
            .get(&track_id)
            .cloned()
            .ok_or(SfuError::UnknownTrack(track_id))?;
        if track.owner != publisher {
            return Err(SfuError::UnauthorizedParticipant(publisher));
        }
        if parsed.header().payload_type != track.payload_type {
            return Err(SfuError::InvalidPayloadType(parsed.header().payload_type));
        }
        let info = inspect_payload(track.codec, parsed.payload(), parsed.header().marker)?;
        let subscription_ids = self
            .track_subscriptions
            .get(&track_id)
            .cloned()
            .unwrap_or_default();
        let mut output = ForwardOutcome::default();
        let mut needs_pli = false;
        for id in subscription_ids {
            let Some(down_track) = self.subscriptions.get_mut(&id) else {
                continue;
            };
            match down_track.forward(now, &track, spatial_layer, info, input)? {
                ForwardDecision::Forwarded { packet, event } => {
                    output.packets.push(packet);
                    output.events.extend(event);
                }
                ForwardDecision::Dropped {
                    waiting_for_keyframe,
                } => needs_pli |= waiting_for_keyframe,
            }
        }
        if needs_pli
            && let Some(target) = self
                .subscriptions
                .values()
                .find(|subscription| {
                    subscription.track_id() == track_id
                        && subscription.selected_spatial()
                            != Some(subscription.target_layer().spatial)
                })
                .and_then(|subscription| track.encoding(subscription.target_layer().spatial))
        {
            output
                .events
                .extend(self.maybe_pli(now, track_id, target.ssrc));
        }
        Ok(output)
    }

    /// Processes authenticated SRTCP plaintext from one subscriber.
    ///
    /// # Errors
    ///
    /// Returns [`SfuError`] for malformed RTCP. Feedback for another subscriber's SSRC is ignored.
    pub fn handle_rtcp(
        &mut self,
        now: Duration,
        subscriber: ParticipantId,
        input: &[u8],
    ) -> Result<ControlOutput, SfuError> {
        let packets = parse_compound(input)?;
        let mut output = ControlOutput::default();
        for packet in packets {
            match packet {
                RtcpPacket::GenericNack(nack) => {
                    let requested = expand_nack(&nack.entries);
                    if let Some(down_track) =
                        self.authorized_down_track_mut(subscriber, nack.media_ssrc)
                    {
                        output
                            .retransmissions
                            .extend(down_track.retransmit(now, &requested));
                    }
                }
                RtcpPacket::PictureLossIndication(pli) => {
                    let upstream = self
                        .authorized_down_track(subscriber, pli.media_ssrc)
                        .and_then(|down_track| {
                            let track = self.tracks.get(&down_track.track_id())?;
                            let spatial = down_track
                                .selected_spatial()
                                .unwrap_or(down_track.target_layer().spatial);
                            track
                                .encoding(spatial)
                                .map(|encoding| (track.id, encoding.ssrc))
                        });
                    if let Some((track_id, media_ssrc)) = upstream {
                        output
                            .events
                            .extend(self.maybe_pli(now, track_id, media_ssrc));
                    }
                }
                RtcpPacket::TransportWideFeedback(feedback) => {
                    output.events.push(SfuEvent::TransportFeedback {
                        subscriber,
                        feedback,
                    });
                }
                _ => {}
            }
        }
        Ok(output)
    }

    fn maybe_pli(&mut self, now: Duration, track_id: TrackId, media_ssrc: u32) -> Option<SfuEvent> {
        if self
            .last_pli
            .get(&media_ssrc)
            .is_some_and(|last| now.saturating_sub(*last) < self.configuration.pli_throttle)
        {
            return None;
        }
        self.last_pli.insert(media_ssrc, now);
        Some(SfuEvent::PictureLossIndication {
            track_id,
            media_ssrc,
        })
    }

    fn authorized_down_track(
        &self,
        subscriber: ParticipantId,
        output_ssrc: u32,
    ) -> Option<&DownTrack> {
        let id = self.output_ssrc_index.get(&output_ssrc)?;
        let down_track = self.subscriptions.get(id)?;
        (down_track.subscriber() == subscriber).then_some(down_track)
    }

    fn authorized_down_track_mut(
        &mut self,
        subscriber: ParticipantId,
        output_ssrc: u32,
    ) -> Option<&mut DownTrack> {
        let id = *self.output_ssrc_index.get(&output_ssrc)?;
        let down_track = self.subscriptions.get_mut(&id)?;
        (down_track.subscriber() == subscriber).then_some(down_track)
    }

    fn remove_subscription(&mut self, id: SubscriptionId) -> bool {
        let Some(subscription) = self.subscriptions.remove(&id) else {
            return false;
        };
        self.output_ssrc_index.remove(&subscription.output_ssrc());
        if let Some(ids) = self.track_subscriptions.get_mut(&subscription.track_id()) {
            ids.retain(|candidate| *candidate != id);
            if ids.is_empty() {
                self.track_subscriptions.remove(&subscription.track_id());
            }
        }
        true
    }
}

fn validate_track(track: &PublishedTrack) -> Result<(), SfuError> {
    if track.encodings.is_empty() || track.clock_rate == 0 {
        return Err(SfuError::InvalidEncodings);
    }
    if !fluvora_rtc_datagram::is_rtcp_mux_safe_payload_type(track.payload_type) {
        return Err(SfuError::InvalidPayloadType(track.payload_type));
    }
    let unique_ssrcs: HashSet<u32> = track
        .encodings
        .iter()
        .map(|encoding| encoding.ssrc)
        .collect();
    let unique_layers: HashSet<u8> = track
        .encodings
        .iter()
        .map(|encoding| encoding.spatial_layer)
        .collect();
    if unique_ssrcs.len() != track.encodings.len() || unique_layers.len() != track.encodings.len() {
        return Err(SfuError::InvalidEncodings);
    }
    Ok(())
}

fn expand_nack(entries: &[fluvora_rtcp::NackEntry]) -> Vec<u16> {
    let mut requested = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries {
        if seen.insert(entry.packet_id) {
            requested.push(entry.packet_id);
        }
        for bit in 0..16 {
            if entry.lost_packet_bitmask & (1 << bit) != 0 {
                let sequence = entry
                    .packet_id
                    .wrapping_add(u16::try_from(bit + 1).unwrap_or_default());
                if seen.insert(sequence) {
                    requested.push(sequence);
                }
            }
        }
    }
    requested
}
