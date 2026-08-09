use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use fluvora_congestion_control::{
    BandwidthEstimator, BandwidthEstimatorConfig, LayerOption, LayerSelector, SentPacket,
};
use fluvora_media_codec::Codec;
use fluvora_rtcp::{Packet as RtcpPacket, PictureLossIndication, encode_compound};
use fluvora_rtp::ExtensionRewrite;
use fluvora_sfu_core::{
    Encoding, Layer, MediaKind, ParticipantId, PublishedTrack, Room, RoomConfig, SfuEvent,
    SubscriptionConfig, SubscriptionId, TrackId,
};

/// Control-plane description of a publisher track.
#[derive(Debug, Clone)]
pub struct PublishTrack {
    /// Room hex identifier.
    pub room_id: String,
    /// Publisher hex identifier.
    pub participant_id: String,
    /// Stable track identifier.
    pub track_id: u64,
    /// Audio or video.
    pub kind: MediaKind,
    /// Payload codec.
    pub codec: Codec,
    /// RTP clock rate.
    pub clock_rate: u32,
    /// Incoming payload type.
    pub payload_type: u8,
    /// Simulcast/SVC inputs.
    pub encodings: Vec<Encoding>,
}

/// Control-plane description of one subscriber down-track.
#[derive(Debug, Clone)]
pub struct SubscribeTrack {
    /// Room hex identifier.
    pub room_id: String,
    /// Subscriber hex identifier.
    pub participant_id: String,
    /// Subscription identifier.
    pub subscription_id: u64,
    /// Source track.
    pub track_id: u64,
    /// Subscriber-visible SSRC.
    pub output_ssrc: u32,
    /// Subscriber-negotiated payload type.
    pub output_payload_type: u8,
    /// Initial spatial layer.
    pub spatial_layer: u8,
    /// Initial temporal layer.
    pub temporal_layer: u8,
    /// First rewritten sequence.
    pub initial_sequence_number: u16,
    /// First rewritten timestamp.
    pub initial_timestamp: u32,
    /// Per-subscriber negotiated header-extension transformations.
    pub extension_rewrites: Vec<ExtensionRewrite>,
    /// Subscriber-negotiated transport-wide sequence extension ID.
    pub transport_wide_extension_id: Option<u8>,
}

/// One clear SFU output and its destination session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfuRoute {
    /// Clear RTP awaiting per-session SRTP protection.
    Rtp {
        session_id: String,
        room_id: String,
        subscriber: ParticipantId,
        subscription_id: SubscriptionId,
        register_twcc: bool,
        packet: Vec<u8>,
    },
    /// Clear RTCP awaiting per-session SRTCP protection.
    Rtcp { session_id: String, packet: Vec<u8> },
    /// Authenticated clear publisher RTP sent only to a loopback packaging worker.
    RecorderRtp {
        /// Local worker UDP socket.
        destination: SocketAddr,
        /// Original publisher RTP packet.
        packet: Vec<u8>,
    },
}

#[derive(Debug)]
struct ManagedRoom {
    room: Room,
    participant_sessions: HashMap<ParticipantId, String>,
    track_owners: HashMap<TrackId, ParticipantId>,
    input_tracks: HashMap<u32, TrackId>,
    subscriptions: HashMap<SubscriptionId, (ParticipantId, TrackId)>,
    congestion: HashMap<ParticipantId, ParticipantCongestion>,
}

#[derive(Debug)]
struct ParticipantCongestion {
    estimator: BandwidthEstimator,
    transport_wide_extension_id: u8,
    subscriptions: HashMap<SubscriptionId, SubscriptionCongestion>,
}

#[derive(Debug)]
struct SubscriptionCongestion {
    selector: LayerSelector,
    options: Vec<LayerOption>,
}

#[derive(Debug, Clone, Copy)]
struct RecordingSink {
    destination: SocketAddr,
    source_ssrc: Option<u32>,
}

/// Concurrent multi-room wrapper around deterministic SFU room cores.
#[derive(Debug, Default)]
pub struct SfuRegistry {
    rooms: RwLock<HashMap<String, Arc<Mutex<ManagedRoom>>>>,
    recording_sinks: RwLock<HashMap<(String, TrackId, SocketAddr), RecordingSink>>,
}

/// Exact, point-in-time SFU allocation counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SfuStats {
    /// Rooms with at least one bound participant transport.
    pub rooms: usize,
    /// Registered source tracks, including transcoded tracks.
    pub publisher_tracks: usize,
    /// Subscriber down-tracks.
    pub subscriber_tracks: usize,
}

impl SfuRegistry {
    /// Returns allocation counts calculated from the registry rather than delta gauges.
    #[must_use]
    pub fn stats(&self) -> SfuStats {
        let rooms = self
            .rooms
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        rooms.values().fold(SfuStats::default(), |mut stats, room| {
            let room = room
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            stats.rooms = stats.rooms.saturating_add(1);
            stats.publisher_tracks = stats
                .publisher_tracks
                .saturating_add(room.track_owners.len());
            stats.subscriber_tracks = stats
                .subscriber_tracks
                .saturating_add(room.subscriptions.len());
            stats
        })
    }

    /// Creates/binds a participant transport. Rebinding after ICE restart replaces the old session.
    ///
    /// # Errors
    ///
    /// Rejects invalid room or participant identifiers.
    pub fn bind_session(
        &self,
        room_id: &str,
        participant_id: &str,
        session_id: &str,
    ) -> Result<(), SfuRuntimeError> {
        validate_identifier(room_id)?;
        let participant = parse_participant(participant_id)?;
        validate_identifier(session_id)?;
        let room = {
            let mut rooms = self
                .rooms
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(rooms.entry(room_id.to_owned()).or_insert_with(|| {
                Arc::new(Mutex::new(ManagedRoom {
                    room: Room::new(RoomConfig::default()),
                    participant_sessions: HashMap::new(),
                    track_owners: HashMap::new(),
                    input_tracks: HashMap::new(),
                    subscriptions: HashMap::new(),
                    congestion: HashMap::new(),
                }))
            }))
        };
        room.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .participant_sessions
            .insert(participant, session_id.to_owned());
        Ok(())
    }

    /// Registers an explicitly negotiated publisher track.
    ///
    /// # Errors
    ///
    /// Rejects unknown rooms/sessions and invalid SFU track constraints.
    pub fn publish(&self, input: PublishTrack) -> Result<(), SfuRuntimeError> {
        let participant = parse_participant(&input.participant_id)?;
        let room = self.room(&input.room_id)?;
        let mut room = room
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !room.participant_sessions.contains_key(&participant) {
            return Err(SfuRuntimeError::UnknownParticipant);
        }
        let track_id = TrackId(input.track_id);
        let input_ssrcs = input
            .encodings
            .iter()
            .map(|encoding| encoding.ssrc)
            .collect::<Vec<_>>();
        room.room
            .publish(PublishedTrack {
                id: track_id,
                owner: participant,
                kind: input.kind,
                codec: input.codec,
                clock_rate: input.clock_rate,
                payload_type: input.payload_type,
                encodings: input.encodings,
            })
            .map_err(|error| SfuRuntimeError::Core(error.to_string()))?;
        room.track_owners.insert(track_id, participant);
        for ssrc in input_ssrcs {
            room.input_tracks.insert(ssrc, track_id);
        }
        Ok(())
    }

    /// Registers a trusted loopback transcoder output as a synthetic publisher track.
    ///
    /// The synthetic track retains the original publisher identity, but its packets are accepted
    /// only by the media-node-owned loopback ingress rather than by an unauthenticated network
    /// socket.
    ///
    /// # Errors
    ///
    /// Rejects unknown publisher sessions and invalid SFU track constraints.
    pub fn publish_transcoded(&self, input: PublishTrack) -> Result<(), SfuRuntimeError> {
        self.publish(input)
    }

    /// Removes a published source or synthetic transcode track and all of its subscriptions.
    #[must_use]
    pub fn unpublish(&self, room_id: &str, track_id: u64) -> bool {
        let Ok(room) = self.room(room_id) else {
            return false;
        };
        let track_id = TrackId(track_id);
        let removed = {
            let mut managed = room
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !managed.room.unpublish(track_id) {
                return false;
            }
            managed.track_owners.remove(&track_id);
            managed.input_tracks.retain(|_, track| *track != track_id);
            let subscriptions = managed
                .subscriptions
                .iter()
                .filter_map(|(subscription, (_, track))| {
                    (*track == track_id).then_some(*subscription)
                })
                .collect::<Vec<_>>();
            for subscription in subscriptions {
                if let Some((subscriber, _)) = managed.subscriptions.remove(&subscription)
                    && let Some(congestion) = managed.congestion.get_mut(&subscriber)
                {
                    congestion.subscriptions.remove(&subscription);
                }
            }
            true
        };
        self.recording_sinks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(sink_room, sink_track, _), _| {
                sink_room != room_id || *sink_track != track_id
            });
        removed
    }

    /// Removes a source track only when it belongs to the authenticated participant.
    ///
    /// # Errors
    ///
    /// Rejects unknown rooms, participants, tracks, or tracks owned by another participant.
    pub fn unpublish_owned(
        &self,
        room_id: &str,
        participant_id: &str,
        track_id: u64,
    ) -> Result<(), SfuRuntimeError> {
        let participant = parse_participant(participant_id)?;
        let room = self.room(room_id)?;
        let track = TrackId(track_id);
        let owner = room
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .track_owners
            .get(&track)
            .copied()
            .ok_or(SfuRuntimeError::UnknownTrack)?;
        if owner != participant {
            return Err(SfuRuntimeError::TrackOwnedByAnotherParticipant);
        }
        if self.unpublish(room_id, track_id) {
            Ok(())
        } else {
            Err(SfuRuntimeError::UnknownTrack)
        }
    }

    /// Adds a loopback-only RTP egress used by the isolated live packager.
    ///
    /// # Errors
    ///
    /// Rejects unknown rooms/tracks and non-loopback destinations.
    pub fn add_recording_sink(
        &self,
        room_id: &str,
        track_id: u64,
        destination: SocketAddr,
    ) -> Result<(), SfuRuntimeError> {
        self.add_recording_sink_for_ssrc(room_id, track_id, destination, None)
    }

    /// Adds a loopback RTP egress restricted to one simulcast source SSRC.
    ///
    /// # Errors
    ///
    /// Rejects unknown tracks, unsafe destinations, or an SSRC outside the source track.
    pub fn add_recording_sink_for_ssrc(
        &self,
        room_id: &str,
        track_id: u64,
        destination: SocketAddr,
        source_ssrc: Option<u32>,
    ) -> Result<(), SfuRuntimeError> {
        if !destination.ip().is_loopback() {
            return Err(SfuRuntimeError::UnsafeRecordingDestination);
        }
        let track_id = TrackId(track_id);
        let room = self.room(room_id)?;
        let managed = room
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(track) = managed.room.published_track(track_id) else {
            return Err(SfuRuntimeError::UnknownTrack);
        };
        if source_ssrc
            .is_some_and(|ssrc| !track.encodings.iter().any(|encoding| encoding.ssrc == ssrc))
        {
            return Err(SfuRuntimeError::UnknownSourceSsrc);
        }
        drop(managed);
        self.recording_sinks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                (room_id.to_owned(), track_id, destination),
                RecordingSink {
                    destination,
                    source_ssrc,
                },
            );
        Ok(())
    }

    /// Removes a live packaging RTP egress.
    #[must_use]
    pub fn remove_recording_sink(&self, room_id: &str, track_id: u64) -> bool {
        let track_id = TrackId(track_id);
        let mut sinks = self
            .recording_sinks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = sinks.len();
        sinks.retain(|(sink_room, sink_track, _), _| {
            sink_room != room_id || *sink_track != track_id
        });
        sinks.len() != before
    }

    /// Removes one exact live packaging or transcode egress.
    #[must_use]
    pub fn remove_recording_sink_destination(
        &self,
        room_id: &str,
        track_id: u64,
        destination: SocketAddr,
    ) -> bool {
        self.recording_sinks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(room_id.to_owned(), TrackId(track_id), destination))
            .is_some()
    }

    /// Builds an immediate PLI for a newly attached recorder/transcoder so it does not start on
    /// an undecodable inter-frame.
    ///
    /// # Errors
    ///
    /// Rejects unknown tracks, source SSRCs, or publishers without a bound transport.
    pub fn request_keyframe(
        &self,
        room_id: &str,
        track_id: u64,
        source_ssrc: Option<u32>,
    ) -> Result<Option<SfuRoute>, SfuRuntimeError> {
        let room = self.room(room_id)?;
        let managed = room
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let track_id = TrackId(track_id);
        let track = managed
            .room
            .published_track(track_id)
            .ok_or(SfuRuntimeError::UnknownTrack)?;
        if track.kind == MediaKind::Audio {
            return Ok(None);
        }
        let media_ssrc = source_ssrc
            .or_else(|| {
                track
                    .encodings
                    .iter()
                    .max_by_key(|encoding| encoding.spatial_layer)
                    .map(|encoding| encoding.ssrc)
            })
            .filter(|ssrc| {
                track
                    .encodings
                    .iter()
                    .any(|encoding| encoding.ssrc == *ssrc)
            })
            .ok_or(SfuRuntimeError::UnknownSourceSsrc)?;
        let owner = managed
            .track_owners
            .get(&track_id)
            .ok_or(SfuRuntimeError::UnknownTrack)?;
        let session_id = managed
            .participant_sessions
            .get(owner)
            .cloned()
            .ok_or(SfuRuntimeError::UnknownParticipant)?;
        let packet = encode_compound(&[RtcpPacket::PictureLossIndication(PictureLossIndication {
            sender_ssrc: 0,
            media_ssrc,
        })])
        .map_err(|error| SfuRuntimeError::Core(error.to_string()))?;
        Ok(Some(SfuRoute::Rtcp { session_id, packet }))
    }

    /// Creates a subscriber down-track.
    ///
    /// # Errors
    ///
    /// Rejects unknown rooms/sessions and invalid SFU subscription constraints.
    pub fn subscribe(&self, input: &SubscribeTrack) -> Result<(), SfuRuntimeError> {
        let participant = parse_participant(&input.participant_id)?;
        let room = self.room(&input.room_id)?;
        let mut room = room
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !room.participant_sessions.contains_key(&participant) {
            return Err(SfuRuntimeError::UnknownParticipant);
        }
        let options = room
            .room
            .published_track(TrackId(input.track_id))
            .ok_or(SfuRuntimeError::UnknownTrack)?
            .encodings
            .iter()
            .map(|encoding| LayerOption {
                layer: Layer {
                    spatial: encoding.spatial_layer,
                    temporal: input.temporal_layer,
                },
                minimum_bitrate_bps: encoding.max_bitrate_bps,
            })
            .collect::<Vec<_>>();
        room.room
            .subscribe(SubscriptionConfig {
                id: SubscriptionId(input.subscription_id),
                subscriber: participant,
                track_id: TrackId(input.track_id),
                output_ssrc: input.output_ssrc,
                output_payload_type: input.output_payload_type,
                initial_layer: Layer {
                    spatial: input.spatial_layer,
                    temporal: input.temporal_layer,
                },
                initial_sequence_number: input.initial_sequence_number,
                initial_timestamp: input.initial_timestamp,
                extension_rewrites: input.extension_rewrites.clone(),
            })
            .map_err(|error| SfuRuntimeError::Core(error.to_string()))?;
        room.subscriptions.insert(
            SubscriptionId(input.subscription_id),
            (participant, TrackId(input.track_id)),
        );
        if let Some(extension_id) = input.transport_wide_extension_id {
            let congestion =
                room.congestion
                    .entry(participant)
                    .or_insert_with(|| ParticipantCongestion {
                        estimator: BandwidthEstimator::new(BandwidthEstimatorConfig::default()),
                        transport_wide_extension_id: extension_id,
                        subscriptions: HashMap::new(),
                    });
            congestion.transport_wide_extension_id = extension_id;
            congestion.subscriptions.insert(
                SubscriptionId(input.subscription_id),
                SubscriptionCongestion {
                    selector: LayerSelector::default(),
                    options,
                },
            );
        }
        Ok(())
    }

    /// Removes one down-track owned by a subscriber.
    ///
    /// # Errors
    ///
    /// Rejects unknown rooms, participants, or subscriptions owned by another participant.
    pub fn unsubscribe(
        &self,
        room_id: &str,
        participant_id: &str,
        subscription_id: u64,
    ) -> Result<(), SfuRuntimeError> {
        let participant = parse_participant(participant_id)?;
        let room = self.room(room_id)?;
        let mut managed = room
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let subscription_id = SubscriptionId(subscription_id);
        let Some((owner, _)) = managed.subscriptions.get(&subscription_id).copied() else {
            return Err(SfuRuntimeError::UnknownSubscription);
        };
        if owner != participant {
            return Err(SfuRuntimeError::UnknownSubscription);
        }
        let _ = managed.room.unsubscribe(subscription_id);
        managed.subscriptions.remove(&subscription_id);
        if let Some(congestion) = managed.congestion.get_mut(&participant) {
            congestion.subscriptions.remove(&subscription_id);
        }
        Ok(())
    }

    /// Removes a participant transport and all media state owned by that participant.
    ///
    /// Replaced transports are protected by matching the current session identifier.
    #[must_use]
    pub fn unbind_session(&self, room_id: &str, participant_id: &str, session_id: &str) -> bool {
        let Ok(participant) = parse_participant(participant_id) else {
            return false;
        };
        let Ok(room) = self.room(room_id) else {
            return false;
        };
        let (owned_tracks, empty) = {
            let mut managed = room
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if managed
                .participant_sessions
                .get(&participant)
                .map(String::as_str)
                != Some(session_id)
            {
                return false;
            }
            managed.participant_sessions.remove(&participant);
            let owned_tracks = managed
                .track_owners
                .iter()
                .filter_map(|(track, owner)| (*owner == participant).then_some(*track))
                .collect::<Vec<_>>();
            let removed_subscriptions = managed
                .subscriptions
                .iter()
                .filter_map(|(subscription, (subscriber, track))| {
                    (*subscriber == participant || owned_tracks.contains(track))
                        .then_some((*subscription, *subscriber))
                })
                .collect::<Vec<_>>();
            for (subscription, subscriber) in removed_subscriptions {
                let _ = managed.room.unsubscribe(subscription);
                managed.subscriptions.remove(&subscription);
                if let Some(congestion) = managed.congestion.get_mut(&subscriber) {
                    congestion.subscriptions.remove(&subscription);
                }
            }
            for track in &owned_tracks {
                let _ = managed.room.unpublish(*track);
                managed.track_owners.remove(track);
            }
            managed
                .input_tracks
                .retain(|_, track| !owned_tracks.contains(track));
            managed.congestion.remove(&participant);
            (owned_tracks, managed.participant_sessions.is_empty())
        };
        {
            let mut sinks = self
                .recording_sinks
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for track in owned_tracks {
                sinks.retain(|(sink_room, sink_track, _), _| {
                    sink_room != room_id || *sink_track != track
                });
            }
        }
        if empty {
            let mut rooms = self
                .rooms
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if rooms.get(room_id).is_some_and(|current| {
                Arc::ptr_eq(current, &room)
                    && current
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .participant_sessions
                        .is_empty()
            }) {
                rooms.remove(room_id);
            }
        }
        true
    }

    /// Applies an adaptive layer target.
    ///
    /// # Errors
    ///
    /// Rejects unknown rooms, participants, subscriptions, or unavailable layers.
    pub fn set_layer(
        &self,
        now: Duration,
        room_id: &str,
        participant_id: &str,
        subscription_id: u64,
        spatial: u8,
        temporal: u8,
    ) -> Result<Vec<SfuRoute>, SfuRuntimeError> {
        let participant = parse_participant(participant_id)?;
        let room = self.room(room_id)?;
        let mut room = room
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let events = room
            .room
            .set_target_layer(
                now,
                participant,
                SubscriptionId(subscription_id),
                Layer { spatial, temporal },
            )
            .map_err(|error| SfuRuntimeError::Core(error.to_string()))?;
        events_to_rtcp(&room, events)
    }

    /// Routes authenticated publisher RTP.
    ///
    /// # Errors
    ///
    /// Rejects unauthorized SSRCs or malformed codec payloads.
    pub fn handle_rtp(
        &self,
        now: Duration,
        room_id: &str,
        participant_id: &str,
        packet: &[u8],
    ) -> Result<Vec<SfuRoute>, SfuRuntimeError> {
        let publisher = parse_participant(participant_id)?;
        let room = self.room(room_id)?;
        let mut room = room
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let output = room
            .room
            .handle_rtp(now, publisher, packet)
            .map_err(|error| SfuRuntimeError::Core(error.to_string()))?;
        let recording_routes = fluvora_rtp::Packet::parse(packet)
            .ok()
            .and_then(|packet| {
                room.input_tracks
                    .get(&packet.header().ssrc)
                    .copied()
                    .map(|track_id| (track_id, packet.header().ssrc))
            })
            .map(|(track_id, packet_ssrc)| {
                self.recording_sinks
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter()
                    .filter_map(|((sink_room, sink_track, _), sink)| {
                        (sink_room == room_id
                            && *sink_track == track_id
                            && sink
                                .source_ssrc
                                .is_none_or(|source_ssrc| source_ssrc == packet_ssrc))
                        .then_some(*sink)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
            .into_iter()
            .map(|sink| SfuRoute::RecorderRtp {
                destination: sink.destination,
                packet: packet.to_vec(),
            });
        let mut routes = output
            .packets
            .into_iter()
            .filter_map(|packet| {
                room.participant_sessions
                    .get(&packet.subscriber)
                    .cloned()
                    .map(|session_id| SfuRoute::Rtp {
                        session_id,
                        room_id: room_id.to_owned(),
                        subscriber: packet.subscriber,
                        subscription_id: packet.subscription_id,
                        register_twcc: true,
                        packet: packet.packet,
                    })
            })
            .collect::<Vec<_>>();
        routes.extend(events_to_rtcp(&room, output.events)?);
        routes.extend(recording_routes);
        Ok(routes)
    }

    /// Routes RTP received from a media-node-owned transcoder loopback socket.
    ///
    /// # Errors
    ///
    /// Rejects packets whose registered SSRC or publisher identity does not match the synthetic
    /// track.
    pub fn handle_transcoded_rtp(
        &self,
        now: Duration,
        room_id: &str,
        publisher_id: &str,
        packet: &[u8],
    ) -> Result<Vec<SfuRoute>, SfuRuntimeError> {
        self.handle_rtp(now, room_id, publisher_id, packet)
    }

    /// Routes authenticated subscriber feedback and cached retransmissions.
    ///
    /// # Errors
    ///
    /// Rejects malformed RTCP or unknown participants.
    pub fn handle_rtcp(
        &self,
        now: Duration,
        room_id: &str,
        participant_id: &str,
        packet: &[u8],
    ) -> Result<Vec<SfuRoute>, SfuRuntimeError> {
        let subscriber = parse_participant(participant_id)?;
        let room = self.room(room_id)?;
        let mut room = room
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let output = room
            .room
            .handle_rtcp(now, subscriber, packet)
            .map_err(|error| SfuRuntimeError::Core(error.to_string()))?;
        let mut adaptive_events = Vec::new();
        for event in output.events {
            match event {
                SfuEvent::TransportFeedback {
                    subscriber,
                    feedback,
                } => {
                    adaptive_events.extend(apply_transport_feedback(
                        &mut room, now, subscriber, &feedback,
                    )?);
                }
                event => adaptive_events.push(event),
            }
        }
        let mut routes = output
            .retransmissions
            .into_iter()
            .filter_map(|packet| {
                room.participant_sessions
                    .get(&packet.subscriber)
                    .cloned()
                    .map(|session_id| SfuRoute::Rtp {
                        session_id,
                        room_id: room_id.to_owned(),
                        subscriber: packet.subscriber,
                        subscription_id: packet.subscription_id,
                        register_twcc: false,
                        packet: packet.packet,
                    })
            })
            .collect::<Vec<_>>();
        routes.extend(events_to_rtcp(&room, adaptive_events)?);
        Ok(routes)
    }

    /// Records a successfully handed-off subscriber RTP packet for TWCC matching.
    #[must_use]
    pub fn register_sent(
        &self,
        now: Duration,
        room_id: &str,
        subscriber: ParticipantId,
        _subscription_id: SubscriptionId,
        packet: &[u8],
    ) -> bool {
        let Ok(room) = self.room(room_id) else {
            return false;
        };
        let mut room = room
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(congestion) = room.congestion.get_mut(&subscriber) else {
            return false;
        };
        let Ok(packet) = fluvora_rtp::Packet::parse(packet) else {
            return false;
        };
        let Some(extension) = packet
            .extensions()
            .iter()
            .find(|extension| extension.id == congestion.transport_wide_extension_id)
        else {
            return false;
        };
        let Ok(sequence) = <[u8; 2]>::try_from(extension.value) else {
            return false;
        };
        congestion.estimator.register_sent(SentPacket {
            sequence_number: u16::from_be_bytes(sequence),
            sent_at: now,
            size_bytes: packet.header_len() + packet.payload().len() + packet.padding_len(),
        });
        true
    }

    fn room(&self, room_id: &str) -> Result<Arc<Mutex<ManagedRoom>>, SfuRuntimeError> {
        self.rooms
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(room_id)
            .cloned()
            .ok_or(SfuRuntimeError::UnknownRoom)
    }
}

fn apply_transport_feedback(
    room: &mut ManagedRoom,
    now: Duration,
    subscriber: ParticipantId,
    feedback: &fluvora_rtcp::TransportWideFeedback,
) -> Result<Vec<SfuEvent>, SfuRuntimeError> {
    let decisions = {
        let Some(congestion) = room.congestion.get_mut(&subscriber) else {
            return Ok(Vec::new());
        };
        let estimate = congestion.estimator.process_feedback(now, feedback);
        if estimate.matched_packets == 0 {
            return Ok(Vec::new());
        }
        congestion
            .subscriptions
            .iter_mut()
            .filter_map(|(subscription_id, subscription)| {
                let previous = subscription.selector.current();
                let selected = subscription.selector.select(
                    now,
                    &subscription.options,
                    estimate.target_bitrate_bps,
                )?;
                (previous != Some(selected)).then_some((*subscription_id, selected))
            })
            .collect::<Vec<_>>()
    };
    let mut events = Vec::new();
    for (subscription_id, layer) in decisions {
        events.extend(
            room.room
                .set_target_layer(now, subscriber, subscription_id, layer)
                .map_err(|error| SfuRuntimeError::Core(error.to_string()))?,
        );
    }
    Ok(events)
}

fn events_to_rtcp(
    room: &ManagedRoom,
    events: Vec<SfuEvent>,
) -> Result<Vec<SfuRoute>, SfuRuntimeError> {
    events
        .into_iter()
        .filter_map(|event| match event {
            SfuEvent::PictureLossIndication {
                track_id,
                media_ssrc,
            } => Some((track_id, media_ssrc)),
            SfuEvent::LayerSwitched { .. } | SfuEvent::TransportFeedback { .. } => None,
        })
        .filter_map(|(track_id, media_ssrc)| {
            let owner = room.track_owners.get(&track_id)?;
            let session_id = room.participant_sessions.get(owner)?.clone();
            Some((session_id, media_ssrc))
        })
        .map(|(session_id, media_ssrc)| {
            let packet =
                encode_compound(&[RtcpPacket::PictureLossIndication(PictureLossIndication {
                    sender_ssrc: 0,
                    media_ssrc,
                })])
                .map_err(|error| SfuRuntimeError::Core(error.to_string()))?;
            Ok(SfuRoute::Rtcp { session_id, packet })
        })
        .collect()
}

fn parse_participant(value: &str) -> Result<ParticipantId, SfuRuntimeError> {
    validate_identifier(value)?;
    u128::from_str_radix(value, 16)
        .map(ParticipantId)
        .map_err(|_| SfuRuntimeError::InvalidIdentifier)
}

fn validate_identifier(value: &str) -> Result<(), SfuRuntimeError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Err(SfuRuntimeError::InvalidIdentifier)
    } else {
        Ok(())
    }
}

/// Media-node SFU runtime error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfuRuntimeError {
    /// Identifier is malformed.
    InvalidIdentifier,
    /// Room has not received a bound session.
    UnknownRoom,
    /// Participant has no bound transport.
    UnknownParticipant,
    /// Published track does not exist.
    UnknownTrack,
    /// Published track belongs to another participant.
    TrackOwnedByAnotherParticipant,
    /// Subscriber down-track does not exist or belongs to another participant.
    UnknownSubscription,
    /// A selected simulcast source SSRC is not part of the track.
    UnknownSourceSsrc,
    /// Recording traffic may only target loopback worker sockets.
    UnsafeRecordingDestination,
    /// Deterministic SFU core rejected the operation.
    Core(String),
}

impl fmt::Display for SfuRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for SfuRuntimeError {}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fluvora_media_codec::Codec;
    use fluvora_rtcp::{Packet as RtcpPacket, TransportWideFeedback, TwccStatus, encode_compound};
    use fluvora_rtp::{ExtensionFormat, OwnedHeaderExtension, PacketBuilder};
    use fluvora_sfu_core::{Encoding, MediaKind};

    use super::{PublishTrack, SfuRegistry, SfuRoute, SubscribeTrack};

    fn adaptive_video_registry() -> SfuRegistry {
        let registry = SfuRegistry::default();
        registry
            .bind_session("room2", "01", "publisher-session")
            .expect("publisher");
        registry
            .bind_session("room2", "02", "subscriber-session")
            .expect("subscriber");
        registry
            .publish(PublishTrack {
                room_id: "room2".to_owned(),
                participant_id: "01".to_owned(),
                track_id: 30,
                kind: MediaKind::Video,
                codec: Codec::Vp8,
                clock_rate: 90_000,
                payload_type: 96,
                encodings: vec![
                    Encoding {
                        ssrc: 300,
                        rid: Some("low".to_owned()),
                        spatial_layer: 0,
                        max_bitrate_bps: 100_000,
                    },
                    Encoding {
                        ssrc: 301,
                        rid: Some("high".to_owned()),
                        spatial_layer: 1,
                        max_bitrate_bps: 1_500_000,
                    },
                ],
            })
            .expect("publish");
        registry
            .subscribe(&SubscribeTrack {
                room_id: "room2".to_owned(),
                participant_id: "02".to_owned(),
                subscription_id: 40,
                track_id: 30,
                output_ssrc: 400,
                output_payload_type: 96,
                spatial_layer: 1,
                temporal_layer: 0,
                initial_sequence_number: 7,
                initial_timestamp: 9,
                extension_rewrites: Vec::new(),
                transport_wide_extension_id: Some(3),
            })
            .expect("subscribe");
        registry
    }

    #[test]
    fn routes_publisher_rtp_to_bound_subscriber() {
        let registry = SfuRegistry::default();
        registry
            .bind_session("room1", "01", "publisher-session")
            .expect("publisher");
        registry
            .bind_session("room1", "02", "subscriber-session")
            .expect("subscriber");
        registry
            .publish(PublishTrack {
                room_id: "room1".to_owned(),
                participant_id: "01".to_owned(),
                track_id: 10,
                kind: MediaKind::Audio,
                codec: Codec::Opus,
                clock_rate: 48_000,
                payload_type: 111,
                encodings: vec![Encoding {
                    ssrc: 99,
                    rid: None,
                    spatial_layer: 0,
                    max_bitrate_bps: 64_000,
                }],
            })
            .expect("publish");
        registry
            .subscribe(&SubscribeTrack {
                room_id: "room1".to_owned(),
                participant_id: "02".to_owned(),
                subscription_id: 20,
                track_id: 10,
                output_ssrc: 199,
                output_payload_type: 111,
                spatial_layer: 0,
                temporal_layer: 0,
                initial_sequence_number: 7,
                initial_timestamp: 9,
                extension_rewrites: Vec::new(),
                transport_wide_extension_id: None,
            })
            .expect("subscribe");
        let packet = PacketBuilder::new(111, 1, 2, 99, &[0x11])
            .marker(true)
            .build()
            .expect("rtp");
        let routes = registry
            .handle_rtp(std::time::Duration::from_secs(1), "room1", "01", &packet)
            .expect("forward");
        assert!(matches!(
            routes.first(),
            Some(SfuRoute::Rtp { session_id, .. }) if session_id == "subscriber-session"
        ));
    }

    #[test]
    fn routes_recording_and_uses_twcc_to_downgrade_layer() {
        let registry = adaptive_video_registry();
        registry
            .add_recording_sink("room2", 30, "127.0.0.1:45000".parse().expect("address"))
            .expect("recording sink");
        assert!(matches!(
            registry.request_keyframe("room2", 30, Some(301)),
            Ok(Some(SfuRoute::Rtcp { session_id, .. }))
                if session_id == "publisher-session"
        ));
        let packet = PacketBuilder::new(96, 1, 2, 301, &[0x10, 0x00])
            .extensions(
                ExtensionFormat::OneByte,
                vec![OwnedHeaderExtension {
                    id: 3,
                    value: 1_u16.to_be_bytes().to_vec(),
                }],
            )
            .marker(true)
            .build()
            .expect("rtp");
        let routes = registry
            .handle_rtp(Duration::from_secs(1), "room2", "01", &packet)
            .expect("forward");
        assert!(routes.iter().any(|route| matches!(
            route,
            SfuRoute::RecorderRtp { destination, .. }
                if destination.port() == 45_000
        )));
        let forwarded = routes
            .iter()
            .find_map(|route| match route {
                SfuRoute::Rtp {
                    subscriber,
                    subscription_id,
                    packet,
                    ..
                } => Some((*subscriber, *subscription_id, packet)),
                _ => None,
            })
            .expect("subscriber route");
        assert!(registry.register_sent(
            Duration::from_secs(1),
            "room2",
            forwarded.0,
            forwarded.1,
            forwarded.2,
        ));
        let feedback =
            encode_compound(&[RtcpPacket::TransportWideFeedback(TransportWideFeedback {
                sender_ssrc: 400,
                media_ssrc: 0,
                base_sequence_number: 1,
                reference_time: 0,
                feedback_packet_count: 1,
                statuses: vec![TwccStatus::ReceivedSmallDelta(1)],
            })])
            .expect("feedback");
        let routes = registry
            .handle_rtcp(Duration::from_secs(2), "room2", "02", &feedback)
            .expect("feedback route");
        assert!(
            routes
                .iter()
                .any(|route| matches!(route, SfuRoute::Rtcp { session_id, .. }
                    if session_id == "publisher-session"))
        );
    }

    #[test]
    fn unbind_removes_participant_subscriptions_and_owned_tracks() {
        let registry = adaptive_video_registry();
        assert!(registry.unbind_session("room2", "02", "subscriber-session"));
        assert!(!registry.unbind_session("room2", "02", "subscriber-session"));
        let packet = PacketBuilder::new(96, 1, 2, 301, &[0x10, 0x00])
            .marker(true)
            .build()
            .expect("RTP");
        let routes = registry
            .handle_rtp(Duration::from_secs(1), "room2", "01", &packet)
            .expect("publisher remains");
        assert!(routes.is_empty());
        assert!(registry.unbind_session("room2", "01", "publisher-session"));
        assert!(
            registry
                .handle_rtp(Duration::from_secs(2), "room2", "01", &packet)
                .is_err()
        );
    }

    #[test]
    fn trusted_transcode_track_reenters_sfu_and_can_be_removed() {
        let registry = SfuRegistry::default();
        registry
            .bind_session("room3", "01", "publisher-session")
            .expect("publisher");
        registry
            .bind_session("room3", "02", "subscriber-session")
            .expect("subscriber");
        registry
            .publish_transcoded(PublishTrack {
                room_id: "room3".to_owned(),
                participant_id: "01".to_owned(),
                track_id: 31,
                kind: MediaKind::Video,
                codec: Codec::Vp8,
                clock_rate: 90_000,
                payload_type: 102,
                encodings: vec![Encoding {
                    ssrc: 310,
                    rid: None,
                    spatial_layer: 0,
                    max_bitrate_bps: 500_000,
                }],
            })
            .expect("synthetic track");
        registry
            .subscribe(&SubscribeTrack {
                room_id: "room3".to_owned(),
                participant_id: "02".to_owned(),
                subscription_id: 41,
                track_id: 31,
                output_ssrc: 410,
                output_payload_type: 102,
                spatial_layer: 0,
                temporal_layer: 0,
                initial_sequence_number: 1,
                initial_timestamp: 2,
                extension_rewrites: Vec::new(),
                transport_wide_extension_id: None,
            })
            .expect("subscribe");
        let packet = PacketBuilder::new(102, 1, 2, 310, &[0x10, 0x00])
            .marker(true)
            .build()
            .expect("RTP");
        assert_eq!(
            registry
                .handle_transcoded_rtp(Duration::from_secs(1), "room3", "01", &packet)
                .expect("route")
                .len(),
            1
        );
        assert!(registry.unpublish("room3", 31));
        assert!(
            registry
                .handle_transcoded_rtp(Duration::from_secs(2), "room3", "01", &packet)
                .is_err()
        );
    }
}
