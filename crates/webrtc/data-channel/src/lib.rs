//! Bounded SCTP-over-DTLS association and WebRTC DCEP implementation.
//!
//! This crate owns the wire protocol. DTLS encryption and datagram transport remain outside it,
//! which makes the state machine deterministic and testable without sockets.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::time::Duration;

const COMMON_HEADER_LEN: usize = 12;
const CHUNK_HEADER_LEN: usize = 4;
const DATA_FIXED_LEN: usize = 16;
const INIT_FIXED_LEN: usize = 20;
const SACK_FIXED_LEN: usize = 16;
const TYPE_DATA: u8 = 0;
const TYPE_INIT: u8 = 1;
const TYPE_INIT_ACK: u8 = 2;
const TYPE_SACK: u8 = 3;
const TYPE_HEARTBEAT: u8 = 4;
const TYPE_HEARTBEAT_ACK: u8 = 5;
const TYPE_ABORT: u8 = 6;
const TYPE_SHUTDOWN: u8 = 7;
const TYPE_SHUTDOWN_ACK: u8 = 8;
const TYPE_COOKIE_ECHO: u8 = 10;
const TYPE_COOKIE_ACK: u8 = 11;
const TYPE_RE_CONFIG: u8 = 130;
const TYPE_FORWARD_TSN: u8 = 192;
const PARAMETER_STATE_COOKIE: u16 = 7;
const PARAMETER_OUTGOING_RESET: u16 = 13;
const PARAMETER_INCOMING_RESET: u16 = 14;
const PARAMETER_RECONFIG_RESPONSE: u16 = 16;
const PARAMETER_FORWARD_TSN_SUPPORTED: u16 = 0xc000;
const RECONFIG_SUCCESS_PERFORMED: u32 = 1;
const RECONFIG_ERROR_BAD_SEQUENCE: u32 = 5;
const FLAG_DATA_END: u8 = 0x01;
const FLAG_DATA_BEGIN: u8 = 0x02;
const FLAG_DATA_UNORDERED: u8 = 0x04;
const PPID_DCEP: u32 = 50;
const PPID_STRING: u32 = 51;
const PPID_BINARY: u32 = 53;
const PPID_STRING_EMPTY: u32 = 56;
const PPID_BINARY_EMPTY: u32 = 57;
const DCEP_ACK: u8 = 0x02;
const DCEP_OPEN: u8 = 0x03;
const DEFAULT_RECEIVE_WINDOW: u32 = 1_048_576;
const MAX_PACKET_BYTES: usize = 1_200;
const MAX_DATA_PAYLOAD: usize = MAX_PACKET_BYTES - COMMON_HEADER_LEN - DATA_FIXED_LEN;
const MAX_PENDING_TSN: usize = 1_024;
const MAX_OUTSTANDING: usize = 256;
const RETRANSMIT_AFTER: Duration = Duration::from_millis(500);
const MAX_RETRANSMISSIONS: u32 = 8;
const MAX_PARTIAL_RETRANSMISSIONS: u32 = 1_024;
const MAX_PARTIAL_LIFETIME: Duration = Duration::from_hours(24);

/// Server-side association limits and deterministic initialization values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssociationConfig {
    /// SCTP port negotiated in SDP, normally 5000.
    pub local_port: u16,
    /// Peer SCTP port negotiated in SDP, normally 5000.
    pub remote_port: u16,
    /// Non-zero local verification tag.
    pub verification_tag: u32,
    /// First locally transmitted TSN.
    pub initial_tsn: u32,
    /// Opaque cookie returned by the peer during the association handshake.
    pub cookie: Vec<u8>,
    /// Maximum simultaneously open data channels.
    pub maximum_channels: usize,
    /// Maximum reassembled user-message bytes.
    pub maximum_message_bytes: usize,
}

impl AssociationConfig {
    /// Validates security and resource bounds.
    ///
    /// # Errors
    ///
    /// Returns [`DataChannelError::InvalidConfiguration`] for zero ports/tags, weak cookies, or
    /// unbounded channel/message limits.
    pub fn validate(&self) -> Result<(), DataChannelError> {
        if self.local_port == 0
            || self.remote_port == 0
            || self.verification_tag == 0
            || !(16..=256).contains(&self.cookie.len())
            || !(1..=1_024).contains(&self.maximum_channels)
            || !(1..=256 * 1_024).contains(&self.maximum_message_bytes)
        {
            return Err(DataChannelError::InvalidConfiguration);
        }
        Ok(())
    }
}

/// SCTP association phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssociationState {
    /// Waiting for a peer INIT.
    Listen,
    /// INIT ACK sent; waiting for COOKIE ECHO.
    CookieWait,
    /// Bidirectional DATA is accepted.
    Established,
    /// Peer aborted or shut down the association.
    Closed,
    /// Reliable delivery exhausted its retry budget.
    Failed,
}

/// WebRTC data-channel payload interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// UTF-8 text.
    Text,
    /// Opaque bytes.
    Binary,
}

/// Delivery policy negotiated by a WebRTC DCEP channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelReliability {
    /// Retransmit until acknowledged or the association-wide safety budget is exhausted.
    Reliable,
    /// Do not retransmit a user message more than the supplied count.
    MaxRetransmissions(u32),
    /// Stop transmitting a user message after the supplied lifetime.
    MaxPacketLifetime(Duration),
}

/// Application-visible association event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssociationEvent {
    /// SCTP cookie exchange completed.
    Established,
    /// A peer-created DCEP channel completed validation.
    ChannelOpened {
        /// SCTP stream identifier.
        stream_id: u16,
        /// UTF-8 channel label.
        label: String,
        /// Optional WebSocket-subprotocol-compatible protocol name.
        protocol: String,
        /// Whether user messages must be delivered in order.
        ordered: bool,
        /// Negotiated reliable or partially reliable delivery policy.
        reliability: ChannelReliability,
    },
    /// A peer reset both directions of a DCEP stream.
    ChannelClosed {
        /// SCTP stream identifier that can now be reused.
        stream_id: u16,
    },
    /// One complete bounded user message.
    Message {
        /// SCTP stream/data-channel identifier.
        stream_id: u16,
        /// Text or binary interpretation.
        kind: MessageKind,
        /// Complete message bytes. Text has already passed UTF-8 validation.
        payload: Vec<u8>,
    },
    /// Peer closed the association.
    Closed,
    /// Reliable DATA could not be acknowledged.
    DeliveryFailed {
        /// Unacknowledged TSN.
        tsn: u32,
    },
    /// A partially reliable user message reached its retry or lifetime limit.
    MessageAbandoned {
        /// SCTP stream/data-channel identifier.
        stream_id: u16,
        /// SCTP stream sequence assigned to the message.
        stream_sequence: u16,
    },
}

/// Packets and application events emitted by one state-machine step.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AssociationOutput {
    /// Complete SCTP packets to write as individual DTLS application messages.
    pub packets: Vec<Vec<u8>>,
    /// Events for the authenticated session owner.
    pub events: Vec<AssociationEvent>,
    /// Number of packets in this output that are retransmitted DATA.
    pub retransmitted_packets: u64,
}

#[derive(Debug, Clone)]
struct Channel {
    ordered: bool,
    reliability: ChannelReliability,
    next_outgoing_sequence: u16,
}

#[derive(Debug, Clone)]
struct DataChunk {
    flags: u8,
    tsn: u32,
    stream_id: u16,
    stream_sequence: u16,
    ppid: u32,
    payload: Vec<u8>,
}

#[derive(Debug, Default)]
struct Reassembly {
    stream_sequence: u16,
    ppid: u32,
    payload: Vec<u8>,
}

#[derive(Debug, Clone)]
struct Outstanding {
    packet: Vec<u8>,
    message_id: u64,
    stream_id: u16,
    stream_sequence: u16,
    ordered: bool,
    reliability: ChannelReliability,
    first_sent_at: Duration,
    sent_at: Duration,
    retransmissions: u32,
}

#[derive(Debug, Clone, Copy)]
struct Abandoned {
    stream_id: u16,
    stream_sequence: u16,
    ordered: bool,
}

#[derive(Debug, Clone)]
struct PendingForwardTsn {
    new_cumulative_tsn: u32,
    skipped_streams: Vec<(u16, u16)>,
    sent_at: Duration,
}

#[derive(Debug, Clone, Copy)]
struct OutboundMessage {
    stream_id: u16,
    stream_sequence: u16,
    ppid: u32,
    ordered: bool,
    reliability: ChannelReliability,
}

/// One bounded server-side SCTP association.
#[derive(Debug)]
pub struct Association {
    config: AssociationConfig,
    state: AssociationState,
    peer_verification_tag: Option<u32>,
    peer_initial_tsn: Option<u32>,
    cumulative_peer_tsn: Option<u32>,
    next_tsn: u32,
    outbound_cumulative_tsn: u32,
    next_message_id: u64,
    next_reconfig_sequence: u32,
    next_peer_reconfig_sequence: Option<u32>,
    peer_outbound_streams: u16,
    channels: HashMap<u16, Channel>,
    pending_data: HashMap<u32, DataChunk>,
    reassembly: HashMap<u16, Reassembly>,
    outstanding: BTreeMap<u32, Outstanding>,
    abandoned: BTreeMap<u32, Abandoned>,
    pending_forward_tsn: Option<PendingForwardTsn>,
    peer_forward_tsn_supported: bool,
}

impl Association {
    /// Creates a server-side association in the listen state.
    ///
    /// # Errors
    ///
    /// Returns an error when configuration bounds are invalid.
    pub fn new(config: AssociationConfig) -> Result<Self, DataChannelError> {
        config.validate()?;
        let next_tsn = config.initial_tsn;
        Ok(Self {
            config,
            state: AssociationState::Listen,
            peer_verification_tag: None,
            peer_initial_tsn: None,
            cumulative_peer_tsn: None,
            next_tsn,
            outbound_cumulative_tsn: next_tsn.wrapping_sub(1),
            next_message_id: 0,
            next_reconfig_sequence: next_tsn,
            next_peer_reconfig_sequence: None,
            peer_outbound_streams: 0,
            channels: HashMap::new(),
            pending_data: HashMap::new(),
            reassembly: HashMap::new(),
            outstanding: BTreeMap::new(),
            abandoned: BTreeMap::new(),
            pending_forward_tsn: None,
            peer_forward_tsn_supported: false,
        })
    }

    /// Current association phase.
    #[must_use]
    pub const fn state(&self) -> AssociationState {
        self.state
    }

    /// Processes one exact SCTP packet obtained from a DTLS application record.
    ///
    /// # Errors
    ///
    /// Rejects malformed, corrupt, misrouted, unauthenticated, oversized, or state-invalid input.
    pub fn handle_packet(
        &mut self,
        now: Duration,
        packet: &[u8],
    ) -> Result<AssociationOutput, DataChannelError> {
        let parsed = ParsedPacket::parse(packet)?;
        if parsed.source_port != self.config.remote_port
            || parsed.destination_port != self.config.local_port
        {
            return Err(DataChannelError::UnexpectedPort);
        }
        let mut output = AssociationOutput::default();
        for chunk in parsed.chunks {
            match chunk.kind {
                TYPE_INIT => self.handle_init(parsed.verification_tag, chunk.value, &mut output)?,
                TYPE_COOKIE_ECHO => {
                    self.handle_cookie_echo(parsed.verification_tag, chunk.value, &mut output)?;
                }
                TYPE_DATA => {
                    self.require_verification_tag(parsed.verification_tag)?;
                    self.handle_data(now, chunk.flags, chunk.value, &mut output)?;
                }
                TYPE_SACK => {
                    self.require_verification_tag(parsed.verification_tag)?;
                    self.handle_sack(chunk.value)?;
                }
                TYPE_HEARTBEAT => {
                    self.require_verification_tag(parsed.verification_tag)?;
                    output.packets.push(self.packet_with_chunk(
                        self.peer_tag()?,
                        TYPE_HEARTBEAT_ACK,
                        0,
                        chunk.value,
                    )?);
                }
                TYPE_ABORT => {
                    self.state = AssociationState::Closed;
                    output.events.push(AssociationEvent::Closed);
                }
                TYPE_SHUTDOWN => {
                    self.require_verification_tag(parsed.verification_tag)?;
                    self.state = AssociationState::Closed;
                    output.packets.push(self.packet_with_chunk(
                        self.peer_tag()?,
                        TYPE_SHUTDOWN_ACK,
                        0,
                        &[],
                    )?);
                    output.events.push(AssociationEvent::Closed);
                }
                TYPE_RE_CONFIG => {
                    self.require_verification_tag(parsed.verification_tag)?;
                    self.handle_reconfiguration(chunk.value, &mut output)?;
                }
                TYPE_FORWARD_TSN => {
                    self.require_verification_tag(parsed.verification_tag)?;
                    self.handle_forward_tsn(now, chunk.value, &mut output)?;
                }
                _ => {}
            }
        }
        Ok(output)
    }

    /// Sends one message on an established peer-created channel.
    ///
    /// # Errors
    ///
    /// Rejects unknown channels, non-established associations, excessive messages, invalid text,
    /// or a full reliable-send window.
    pub fn send_message(
        &mut self,
        now: Duration,
        stream_id: u16,
        kind: MessageKind,
        payload: &[u8],
    ) -> Result<Vec<Vec<u8>>, DataChannelError> {
        if self.state != AssociationState::Established {
            return Err(DataChannelError::AssociationNotEstablished);
        }
        if payload.len() > self.config.maximum_message_bytes {
            return Err(DataChannelError::MessageTooLarge);
        }
        if kind == MessageKind::Text {
            std::str::from_utf8(payload).map_err(|_| DataChannelError::InvalidUtf8)?;
        }
        let channel = self
            .channels
            .get_mut(&stream_id)
            .ok_or(DataChannelError::UnknownChannel)?;
        let ordered = channel.ordered;
        let reliability = channel.reliability;
        let sequence = channel.next_outgoing_sequence;
        channel.next_outgoing_sequence = channel.next_outgoing_sequence.wrapping_add(1);
        let (ppid, wire_payload) = match (kind, payload.is_empty()) {
            (MessageKind::Text, false) => (PPID_STRING, payload),
            (MessageKind::Binary, false) => (PPID_BINARY, payload),
            (MessageKind::Text, true) => (PPID_STRING_EMPTY, &[0][..]),
            (MessageKind::Binary, true) => (PPID_BINARY_EMPTY, &[0][..]),
        };
        self.enqueue_message(
            now,
            OutboundMessage {
                stream_id,
                stream_sequence: sequence,
                ppid,
                ordered,
                reliability,
            },
            wire_payload,
        )
    }

    /// Retransmits overdue DATA, abandons expired partial-reliability messages, and reports
    /// exhausted reliable delivery.
    #[must_use]
    pub fn tick(&mut self, now: Duration) -> AssociationOutput {
        if self.state != AssociationState::Established {
            return AssociationOutput::default();
        }
        let mut output = AssociationOutput::default();
        let mut abandoned_messages = BTreeMap::<u64, (u16, u16)>::new();
        for (tsn, outstanding) in &mut self.outstanding {
            let overdue = now.saturating_sub(outstanding.sent_at) >= RETRANSMIT_AFTER;
            let should_abandon = match outstanding.reliability {
                ChannelReliability::Reliable => false,
                ChannelReliability::MaxRetransmissions(maximum) => {
                    overdue && outstanding.retransmissions >= maximum
                }
                ChannelReliability::MaxPacketLifetime(lifetime) => {
                    now.saturating_sub(outstanding.first_sent_at) >= lifetime
                }
            };
            if should_abandon {
                abandoned_messages
                    .entry(outstanding.message_id)
                    .or_insert((outstanding.stream_id, outstanding.stream_sequence));
                continue;
            }
            if !overdue {
                continue;
            }
            if outstanding.reliability == ChannelReliability::Reliable
                && outstanding.retransmissions >= MAX_RETRANSMISSIONS
            {
                self.state = AssociationState::Failed;
                output
                    .events
                    .push(AssociationEvent::DeliveryFailed { tsn: *tsn });
                break;
            }
            outstanding.sent_at = now;
            outstanding.retransmissions = outstanding.retransmissions.saturating_add(1);
            output.packets.push(outstanding.packet.clone());
            output.retransmitted_packets = output.retransmitted_packets.saturating_add(1);
        }
        if self.state == AssociationState::Failed {
            return output;
        }
        for (message_id, (stream_id, stream_sequence)) in abandoned_messages {
            let tsns = self
                .outstanding
                .iter()
                .filter_map(|(tsn, outstanding)| {
                    (outstanding.message_id == message_id).then_some(*tsn)
                })
                .collect::<Vec<_>>();
            for tsn in tsns {
                if let Some(outstanding) = self.outstanding.remove(&tsn) {
                    self.abandoned.insert(
                        tsn,
                        Abandoned {
                            stream_id: outstanding.stream_id,
                            stream_sequence: outstanding.stream_sequence,
                            ordered: outstanding.ordered,
                        },
                    );
                }
            }
            output.events.push(AssociationEvent::MessageAbandoned {
                stream_id,
                stream_sequence,
            });
        }
        if self.peer_forward_tsn_supported
            && let Ok(Some(packet)) = self.build_forward_tsn(now)
        {
            output.packets.push(packet);
        }
        output
    }

    fn handle_init(
        &mut self,
        verification_tag: u32,
        value: &[u8],
        output: &mut AssociationOutput,
    ) -> Result<(), DataChannelError> {
        if verification_tag != 0 || value.len() < INIT_FIXED_LEN - CHUNK_HEADER_LEN {
            return Err(DataChannelError::MalformedChunk);
        }
        let peer_tag = read_u32(value, 0)?;
        let outbound_streams = read_u16(value, 8)?;
        let inbound_streams = read_u16(value, 10)?;
        let initial_tsn = read_u32(value, 12)?;
        if peer_tag == 0 || outbound_streams == 0 || inbound_streams == 0 {
            return Err(DataChannelError::MalformedChunk);
        }
        let parameters = parse_parameters(&value[16..])?;
        self.peer_forward_tsn_supported = parameters.iter().any(|parameter| {
            parameter.kind == PARAMETER_FORWARD_TSN_SUPPORTED && parameter.value.is_empty()
        });
        self.peer_verification_tag = Some(peer_tag);
        self.peer_initial_tsn = Some(initial_tsn);
        self.cumulative_peer_tsn = Some(initial_tsn.wrapping_sub(1));
        self.peer_outbound_streams = outbound_streams;
        self.pending_data.clear();
        self.reassembly.clear();
        self.outstanding.clear();
        self.abandoned.clear();
        self.pending_forward_tsn = None;
        self.channels.clear();
        self.next_tsn = self.config.initial_tsn;
        self.outbound_cumulative_tsn = self.config.initial_tsn.wrapping_sub(1);
        self.next_message_id = 0;
        self.next_reconfig_sequence = self.config.initial_tsn;
        self.next_peer_reconfig_sequence = Some(initial_tsn);
        self.state = AssociationState::CookieWait;

        let streams = u16::try_from(self.config.maximum_channels)
            .unwrap_or(u16::MAX)
            .min(inbound_streams)
            .max(1);
        let mut init_ack = Vec::with_capacity(64);
        init_ack.extend_from_slice(&self.config.verification_tag.to_be_bytes());
        init_ack.extend_from_slice(&DEFAULT_RECEIVE_WINDOW.to_be_bytes());
        init_ack.extend_from_slice(&streams.to_be_bytes());
        init_ack.extend_from_slice(&streams.to_be_bytes());
        init_ack.extend_from_slice(&self.config.initial_tsn.to_be_bytes());
        push_parameter(&mut init_ack, PARAMETER_STATE_COOKIE, &self.config.cookie)?;
        if self.peer_forward_tsn_supported {
            push_parameter(&mut init_ack, PARAMETER_FORWARD_TSN_SUPPORTED, &[])?;
        }
        output
            .packets
            .push(self.packet_with_chunk(peer_tag, TYPE_INIT_ACK, 0, &init_ack)?);
        Ok(())
    }

    fn handle_cookie_echo(
        &mut self,
        verification_tag: u32,
        cookie: &[u8],
        output: &mut AssociationOutput,
    ) -> Result<(), DataChannelError> {
        if self.state != AssociationState::CookieWait {
            return Err(DataChannelError::UnexpectedState);
        }
        self.require_verification_tag(verification_tag)?;
        if !constant_time_equal(cookie, &self.config.cookie) {
            return Err(DataChannelError::InvalidCookie);
        }
        self.state = AssociationState::Established;
        output
            .packets
            .push(self.packet_with_chunk(self.peer_tag()?, TYPE_COOKIE_ACK, 0, &[])?);
        output.events.push(AssociationEvent::Established);
        Ok(())
    }

    fn handle_data(
        &mut self,
        now: Duration,
        flags: u8,
        value: &[u8],
        output: &mut AssociationOutput,
    ) -> Result<(), DataChannelError> {
        if self.state != AssociationState::Established || value.len() < DATA_FIXED_LEN - 4 {
            return Err(DataChannelError::UnexpectedState);
        }
        let data = DataChunk {
            flags,
            tsn: read_u32(value, 0)?,
            stream_id: read_u16(value, 4)?,
            stream_sequence: read_u16(value, 6)?,
            ppid: read_u32(value, 8)?,
            payload: value[12..].to_vec(),
        };
        let deliverable = self.accept_data(data)?;
        for data in deliverable {
            self.process_data_message(now, data, output)?;
        }
        output.packets.push(self.build_sack()?);
        Ok(())
    }

    fn handle_reconfiguration(
        &mut self,
        value: &[u8],
        output: &mut AssociationOutput,
    ) -> Result<(), DataChannelError> {
        if self.state != AssociationState::Established || value.is_empty() {
            return Err(DataChannelError::UnexpectedState);
        }
        let parameters = parse_parameters(value)?;
        if parameters.is_empty() || parameters.len() > 2 {
            return Err(DataChannelError::MalformedChunk);
        }
        let mut response = Vec::new();
        for parameter in parameters {
            match parameter.kind {
                PARAMETER_OUTGOING_RESET => {
                    self.handle_outgoing_reset(parameter.value, &mut response, output)?;
                }
                PARAMETER_INCOMING_RESET => {
                    self.handle_incoming_reset(parameter.value, &mut response, output)?;
                }
                PARAMETER_RECONFIG_RESPONSE => {
                    if !matches!(parameter.value.len(), 8 | 16) {
                        return Err(DataChannelError::MalformedChunk);
                    }
                }
                _ => break,
            }
        }
        if !response.is_empty() {
            output.packets.push(self.packet_with_chunk(
                self.peer_tag()?,
                TYPE_RE_CONFIG,
                0,
                &response,
            )?);
        }
        Ok(())
    }

    fn handle_outgoing_reset(
        &mut self,
        value: &[u8],
        response: &mut Vec<u8>,
        output: &mut AssociationOutput,
    ) -> Result<(), DataChannelError> {
        if value.len() < 12 || !(value.len() - 12).is_multiple_of(2) {
            return Err(DataChannelError::MalformedChunk);
        }
        let request_sequence = read_u32(value, 0)?;
        let expected = self
            .next_peer_reconfig_sequence
            .ok_or(DataChannelError::UnexpectedState)?;
        let result = if request_sequence == expected {
            self.next_peer_reconfig_sequence = Some(expected.wrapping_add(1));
            let streams = parse_stream_numbers(&value[12..], self.peer_outbound_streams)?;
            self.close_streams(streams.as_deref(), output);
            RECONFIG_SUCCESS_PERFORMED
        } else {
            RECONFIG_ERROR_BAD_SEQUENCE
        };
        let mut response_value = Vec::with_capacity(8);
        response_value.extend_from_slice(&request_sequence.to_be_bytes());
        response_value.extend_from_slice(&result.to_be_bytes());
        push_parameter(response, PARAMETER_RECONFIG_RESPONSE, &response_value)
    }

    fn handle_incoming_reset(
        &mut self,
        value: &[u8],
        response: &mut Vec<u8>,
        output: &mut AssociationOutput,
    ) -> Result<(), DataChannelError> {
        if value.len() < 4 || !(value.len() - 4).is_multiple_of(2) {
            return Err(DataChannelError::MalformedChunk);
        }
        let peer_request_sequence = read_u32(value, 0)?;
        let expected = self
            .next_peer_reconfig_sequence
            .ok_or(DataChannelError::UnexpectedState)?;
        if peer_request_sequence != expected {
            let mut response_value = Vec::with_capacity(8);
            response_value.extend_from_slice(&peer_request_sequence.to_be_bytes());
            response_value.extend_from_slice(&RECONFIG_ERROR_BAD_SEQUENCE.to_be_bytes());
            return push_parameter(response, PARAMETER_RECONFIG_RESPONSE, &response_value);
        }
        self.next_peer_reconfig_sequence = Some(expected.wrapping_add(1));
        let streams = parse_stream_numbers(&value[4..], self.peer_outbound_streams)?;
        self.close_streams(streams.as_deref(), output);

        let mut outgoing = Vec::with_capacity(value.len() + 8);
        outgoing.extend_from_slice(&self.next_reconfig_sequence.to_be_bytes());
        outgoing.extend_from_slice(&peer_request_sequence.to_be_bytes());
        outgoing.extend_from_slice(&self.next_tsn.wrapping_sub(1).to_be_bytes());
        if let Some(streams) = streams {
            for stream_id in streams {
                outgoing.extend_from_slice(&stream_id.to_be_bytes());
            }
        }
        self.next_reconfig_sequence = self.next_reconfig_sequence.wrapping_add(1);
        push_parameter(response, PARAMETER_OUTGOING_RESET, &outgoing)
    }

    fn close_streams(&mut self, streams: Option<&[u16]>, output: &mut AssociationOutput) {
        let stream_ids = streams.map_or_else(
            || self.channels.keys().copied().collect::<Vec<_>>(),
            <[u16]>::to_vec,
        );
        for stream_id in stream_ids {
            if self.channels.remove(&stream_id).is_some() {
                self.reassembly.remove(&stream_id);
                output
                    .events
                    .push(AssociationEvent::ChannelClosed { stream_id });
            }
        }
    }

    fn accept_data(&mut self, data: DataChunk) -> Result<Vec<DataChunk>, DataChannelError> {
        let cumulative = self
            .cumulative_peer_tsn
            .ok_or(DataChannelError::UnexpectedState)?;
        let delta = data.tsn.wrapping_sub(cumulative);
        if delta == 0 || delta >= (1 << 31) {
            return Ok(Vec::new());
        }
        if delta > u32::try_from(MAX_PENDING_TSN).unwrap_or(u32::MAX) {
            return Err(DataChannelError::ReceiveWindowExceeded);
        }
        if self.pending_data.len() >= MAX_PENDING_TSN && !self.pending_data.contains_key(&data.tsn)
        {
            return Err(DataChannelError::ReceiveWindowExceeded);
        }
        self.pending_data.entry(data.tsn).or_insert(data);
        let mut deliverable = Vec::new();
        let mut next = cumulative.wrapping_add(1);
        while let Some(data) = self.pending_data.remove(&next) {
            deliverable.push(data);
            self.cumulative_peer_tsn = Some(next);
            next = next.wrapping_add(1);
        }
        Ok(deliverable)
    }

    fn process_data_message(
        &mut self,
        now: Duration,
        data: DataChunk,
        output: &mut AssociationOutput,
    ) -> Result<(), DataChannelError> {
        if data.stream_id >= self.peer_outbound_streams {
            return Err(DataChannelError::UnknownChannel);
        }
        let begin = data.flags & FLAG_DATA_BEGIN != 0;
        let end = data.flags & FLAG_DATA_END != 0;
        let payload = if begin && end {
            data.payload
        } else {
            self.reassemble(&data, begin, end)?.unwrap_or_default()
        };
        if !end {
            return Ok(());
        }
        match data.ppid {
            PPID_DCEP => self.handle_dcep(now, data.stream_id, &payload, output),
            PPID_STRING | PPID_BINARY | PPID_STRING_EMPTY | PPID_BINARY_EMPTY => {
                self.handle_user_message(data.stream_id, data.ppid, payload, output)
            }
            _ => Err(DataChannelError::UnsupportedPpid(data.ppid)),
        }
    }

    fn reassemble(
        &mut self,
        data: &DataChunk,
        begin: bool,
        end: bool,
    ) -> Result<Option<Vec<u8>>, DataChannelError> {
        if begin {
            self.reassembly.insert(
                data.stream_id,
                Reassembly {
                    stream_sequence: data.stream_sequence,
                    ppid: data.ppid,
                    payload: Vec::new(),
                },
            );
        }
        let assembly = self
            .reassembly
            .get_mut(&data.stream_id)
            .ok_or(DataChannelError::InvalidFragmentSequence)?;
        if assembly.stream_sequence != data.stream_sequence || assembly.ppid != data.ppid {
            return Err(DataChannelError::InvalidFragmentSequence);
        }
        if assembly.payload.len().saturating_add(data.payload.len())
            > self.config.maximum_message_bytes
        {
            self.reassembly.remove(&data.stream_id);
            return Err(DataChannelError::MessageTooLarge);
        }
        assembly.payload.extend_from_slice(&data.payload);
        if end {
            Ok(self
                .reassembly
                .remove(&data.stream_id)
                .map(|assembly| assembly.payload))
        } else {
            Ok(None)
        }
    }

    fn handle_dcep(
        &mut self,
        now: Duration,
        stream_id: u16,
        payload: &[u8],
        output: &mut AssociationOutput,
    ) -> Result<(), DataChannelError> {
        if payload.first() != Some(&DCEP_OPEN)
            || payload.len() < 12
            || !stream_id.is_multiple_of(2)
            || self.channels.contains_key(&stream_id)
            || self.channels.len() >= self.config.maximum_channels
        {
            return Err(DataChannelError::InvalidDcep);
        }
        let channel_type = payload[1];
        let reliability_parameter = read_u32(payload, 4)?;
        let (ordered, reliability) = match channel_type {
            0x00 => (true, ChannelReliability::Reliable),
            0x80 => (false, ChannelReliability::Reliable),
            0x01 | 0x81
                if self.peer_forward_tsn_supported
                    && reliability_parameter <= MAX_PARTIAL_RETRANSMISSIONS =>
            {
                (
                    channel_type == 0x01,
                    ChannelReliability::MaxRetransmissions(reliability_parameter),
                )
            }
            0x02 | 0x82
                if self.peer_forward_tsn_supported
                    && Duration::from_millis(u64::from(reliability_parameter))
                        <= MAX_PARTIAL_LIFETIME =>
            {
                (
                    channel_type == 0x02,
                    ChannelReliability::MaxPacketLifetime(Duration::from_millis(u64::from(
                        reliability_parameter,
                    ))),
                )
            }
            _ => return Err(DataChannelError::InvalidDcep),
        };
        let label_length = usize::from(read_u16(payload, 8)?);
        let protocol_length = usize::from(read_u16(payload, 10)?);
        let expected = 12usize
            .checked_add(label_length)
            .and_then(|length| length.checked_add(protocol_length))
            .ok_or(DataChannelError::MessageTooLarge)?;
        if expected != payload.len() || label_length > 1_024 || protocol_length > 1_024 {
            return Err(DataChannelError::InvalidDcep);
        }
        let label = std::str::from_utf8(&payload[12..12 + label_length])
            .map_err(|_| DataChannelError::InvalidUtf8)?
            .to_owned();
        let protocol = std::str::from_utf8(&payload[12 + label_length..])
            .map_err(|_| DataChannelError::InvalidUtf8)?
            .to_owned();
        self.channels.insert(
            stream_id,
            Channel {
                ordered,
                reliability,
                next_outgoing_sequence: 0,
            },
        );
        output.packets.extend(self.enqueue_message(
            now,
            OutboundMessage {
                stream_id,
                stream_sequence: 0,
                ppid: PPID_DCEP,
                ordered: true,
                reliability: ChannelReliability::Reliable,
            },
            &[DCEP_ACK],
        )?);
        output.events.push(AssociationEvent::ChannelOpened {
            stream_id,
            label,
            protocol,
            ordered,
            reliability,
        });
        Ok(())
    }

    fn handle_user_message(
        &self,
        stream_id: u16,
        ppid: u32,
        mut payload: Vec<u8>,
        output: &mut AssociationOutput,
    ) -> Result<(), DataChannelError> {
        if !self.channels.contains_key(&stream_id) {
            return Err(DataChannelError::UnknownChannel);
        }
        let kind = match ppid {
            PPID_STRING | PPID_STRING_EMPTY => MessageKind::Text,
            PPID_BINARY | PPID_BINARY_EMPTY => MessageKind::Binary,
            _ => return Err(DataChannelError::UnsupportedPpid(ppid)),
        };
        if matches!(ppid, PPID_STRING_EMPTY | PPID_BINARY_EMPTY) {
            payload.clear();
        }
        if kind == MessageKind::Text {
            std::str::from_utf8(&payload).map_err(|_| DataChannelError::InvalidUtf8)?;
        }
        if payload.len() > self.config.maximum_message_bytes {
            return Err(DataChannelError::MessageTooLarge);
        }
        output.events.push(AssociationEvent::Message {
            stream_id,
            kind,
            payload,
        });
        Ok(())
    }

    fn enqueue_message(
        &mut self,
        now: Duration,
        message: OutboundMessage,
        payload: &[u8],
    ) -> Result<Vec<Vec<u8>>, DataChannelError> {
        let fragments = payload.chunks(MAX_DATA_PAYLOAD).collect::<Vec<_>>();
        let flight_span = self
            .next_tsn
            .wrapping_sub(self.outbound_cumulative_tsn)
            .saturating_sub(1) as usize;
        if self.outstanding.len().saturating_add(fragments.len()) > MAX_OUTSTANDING
            || flight_span.saturating_add(fragments.len()) > MAX_OUTSTANDING
        {
            return Err(DataChannelError::SendWindowFull);
        }
        let fragment_count = fragments.len();
        let message_id = self.next_message_id;
        self.next_message_id = self.next_message_id.wrapping_add(1);
        let mut packets = Vec::with_capacity(fragment_count);
        for (index, fragment) in fragments.into_iter().enumerate() {
            let mut flags = u8::from(!message.ordered) * FLAG_DATA_UNORDERED;
            if index == 0 {
                flags |= FLAG_DATA_BEGIN;
            }
            if index + 1 == fragment_count {
                flags |= FLAG_DATA_END;
            }
            let tsn = self.next_tsn;
            self.next_tsn = self.next_tsn.wrapping_add(1);
            let mut value = Vec::with_capacity(12 + fragment.len());
            value.extend_from_slice(&tsn.to_be_bytes());
            value.extend_from_slice(&message.stream_id.to_be_bytes());
            value.extend_from_slice(&message.stream_sequence.to_be_bytes());
            value.extend_from_slice(&message.ppid.to_be_bytes());
            value.extend_from_slice(fragment);
            let packet = self.packet_with_chunk(self.peer_tag()?, TYPE_DATA, flags, &value)?;
            self.outstanding.insert(
                tsn,
                Outstanding {
                    packet: packet.clone(),
                    message_id,
                    stream_id: message.stream_id,
                    stream_sequence: message.stream_sequence,
                    ordered: message.ordered,
                    reliability: message.reliability,
                    first_sent_at: now,
                    sent_at: now,
                    retransmissions: 0,
                },
            );
            packets.push(packet);
        }
        Ok(packets)
    }

    fn handle_sack(&mut self, value: &[u8]) -> Result<(), DataChannelError> {
        if value.len() < SACK_FIXED_LEN - 4 {
            return Err(DataChannelError::MalformedChunk);
        }
        let cumulative = read_u32(value, 0)?;
        let gap_count = usize::from(read_u16(value, 8)?);
        let duplicate_count = usize::from(read_u16(value, 10)?);
        let expected = 12usize
            .checked_add(gap_count.saturating_mul(4))
            .and_then(|length| length.checked_add(duplicate_count.saturating_mul(4)))
            .ok_or(DataChannelError::MalformedChunk)?;
        if expected > value.len() {
            return Err(DataChannelError::MalformedChunk);
        }
        if !sequence_less_or_equal(cumulative, self.outbound_cumulative_tsn) {
            self.outbound_cumulative_tsn = cumulative;
        }
        self.outstanding
            .retain(|tsn, _| !sequence_less_or_equal(*tsn, cumulative));
        self.abandoned
            .retain(|tsn, _| !sequence_less_or_equal(*tsn, cumulative));
        if self
            .pending_forward_tsn
            .as_ref()
            .is_some_and(|pending| sequence_less_or_equal(pending.new_cumulative_tsn, cumulative))
        {
            self.pending_forward_tsn = None;
        }
        for index in 0..gap_count {
            let offset = 12 + index * 4;
            let start = u32::from(read_u16(value, offset)?);
            let end = u32::from(read_u16(value, offset + 2)?);
            if start == 0 || end < start {
                return Err(DataChannelError::MalformedChunk);
            }
            for delta in start..=end {
                self.outstanding.remove(&cumulative.wrapping_add(delta));
            }
        }
        Ok(())
    }

    fn build_forward_tsn(&mut self, now: Duration) -> Result<Option<Vec<u8>>, DataChannelError> {
        let mut next = self.outbound_cumulative_tsn.wrapping_add(1);
        let mut saw_abandoned = false;
        let mut skipped_streams = BTreeMap::<u16, u16>::new();
        while next != self.next_tsn {
            if self.outstanding.contains_key(&next) {
                break;
            }
            if let Some(abandoned) = self.abandoned.get(&next) {
                saw_abandoned = true;
                if abandoned.ordered {
                    skipped_streams.insert(abandoned.stream_id, abandoned.stream_sequence);
                }
            }
            next = next.wrapping_add(1);
        }
        if !saw_abandoned {
            return Ok(None);
        }
        let new_cumulative_tsn = next.wrapping_sub(1);
        let skipped_streams = skipped_streams.into_iter().collect::<Vec<_>>();
        let should_send = self.pending_forward_tsn.as_ref().is_none_or(|pending| {
            pending.new_cumulative_tsn != new_cumulative_tsn
                || pending.skipped_streams != skipped_streams
                || now.saturating_sub(pending.sent_at) >= RETRANSMIT_AFTER
        });
        if !should_send {
            return Ok(None);
        }
        let mut value = Vec::with_capacity(4 + skipped_streams.len() * 4);
        value.extend_from_slice(&new_cumulative_tsn.to_be_bytes());
        for (stream_id, stream_sequence) in &skipped_streams {
            value.extend_from_slice(&stream_id.to_be_bytes());
            value.extend_from_slice(&stream_sequence.to_be_bytes());
        }
        let packet = self.packet_with_chunk(self.peer_tag()?, TYPE_FORWARD_TSN, 0, &value)?;
        self.pending_forward_tsn = Some(PendingForwardTsn {
            new_cumulative_tsn,
            skipped_streams,
            sent_at: now,
        });
        Ok(Some(packet))
    }

    fn handle_forward_tsn(
        &mut self,
        now: Duration,
        value: &[u8],
        output: &mut AssociationOutput,
    ) -> Result<(), DataChannelError> {
        if self.state != AssociationState::Established
            || !self.peer_forward_tsn_supported
            || value.len() < 4
            || !(value.len() - 4).is_multiple_of(4)
        {
            return Err(DataChannelError::MalformedChunk);
        }
        let new_cumulative_tsn = read_u32(value, 0)?;
        let current = self
            .cumulative_peer_tsn
            .ok_or(DataChannelError::UnexpectedState)?;
        if sequence_less_or_equal(new_cumulative_tsn, current) {
            output.packets.push(self.build_sack()?);
            return Ok(());
        }
        for offset in (4..value.len()).step_by(4) {
            let stream_id = read_u16(value, offset)?;
            let stream_sequence = read_u16(value, offset + 2)?;
            if let Some(reassembly) = self.reassembly.get(&stream_id)
                && sequence_less_or_equal_u16(reassembly.stream_sequence, stream_sequence)
            {
                self.reassembly.remove(&stream_id);
            }
        }
        self.pending_data
            .retain(|tsn, _| !sequence_less_or_equal(*tsn, new_cumulative_tsn));
        self.cumulative_peer_tsn = Some(new_cumulative_tsn);
        let mut next = new_cumulative_tsn.wrapping_add(1);
        while let Some(data) = self.pending_data.remove(&next) {
            self.process_data_message(now, data, output)?;
            self.cumulative_peer_tsn = Some(next);
            next = next.wrapping_add(1);
        }
        output.packets.push(self.build_sack()?);
        Ok(())
    }

    fn build_sack(&self) -> Result<Vec<u8>, DataChannelError> {
        let cumulative = self
            .cumulative_peer_tsn
            .ok_or(DataChannelError::UnexpectedState)?;
        let mut deltas = self
            .pending_data
            .keys()
            .map(|tsn| tsn.wrapping_sub(cumulative))
            .filter_map(|delta| u16::try_from(delta).ok())
            .collect::<Vec<_>>();
        deltas.sort_unstable();
        let mut gaps = Vec::<(u16, u16)>::new();
        for delta in deltas {
            if let Some((_, end)) = gaps.last_mut()
                && delta == end.saturating_add(1)
            {
                *end = delta;
            } else {
                gaps.push((delta, delta));
            }
        }
        let mut value = Vec::with_capacity(12 + gaps.len() * 4);
        value.extend_from_slice(&cumulative.to_be_bytes());
        value.extend_from_slice(&DEFAULT_RECEIVE_WINDOW.to_be_bytes());
        value.extend_from_slice(&u16::try_from(gaps.len()).unwrap_or(u16::MAX).to_be_bytes());
        value.extend_from_slice(&0_u16.to_be_bytes());
        for (start, end) in gaps {
            value.extend_from_slice(&start.to_be_bytes());
            value.extend_from_slice(&end.to_be_bytes());
        }
        self.packet_with_chunk(self.peer_tag()?, TYPE_SACK, 0, &value)
    }

    fn require_verification_tag(&self, supplied: u32) -> Result<(), DataChannelError> {
        if supplied == self.config.verification_tag {
            Ok(())
        } else {
            Err(DataChannelError::InvalidVerificationTag)
        }
    }

    fn peer_tag(&self) -> Result<u32, DataChannelError> {
        self.peer_verification_tag
            .ok_or(DataChannelError::UnexpectedState)
    }

    fn packet_with_chunk(
        &self,
        verification_tag: u32,
        kind: u8,
        flags: u8,
        value: &[u8],
    ) -> Result<Vec<u8>, DataChannelError> {
        build_packet(
            self.config.local_port,
            self.config.remote_port,
            verification_tag,
            kind,
            flags,
            value,
        )
    }
}

#[derive(Debug)]
struct ParsedChunk<'a> {
    kind: u8,
    flags: u8,
    value: &'a [u8],
}

#[derive(Debug)]
struct ParsedPacket<'a> {
    source_port: u16,
    destination_port: u16,
    verification_tag: u32,
    chunks: Vec<ParsedChunk<'a>>,
}

impl<'a> ParsedPacket<'a> {
    fn parse(input: &'a [u8]) -> Result<Self, DataChannelError> {
        if !(COMMON_HEADER_LEN + CHUNK_HEADER_LEN..=65_535).contains(&input.len()) {
            return Err(DataChannelError::MalformedPacket);
        }
        let expected_checksum = read_u32_le(input, 8)?;
        if checksum(input) != expected_checksum {
            return Err(DataChannelError::InvalidChecksum);
        }
        let mut chunks = Vec::new();
        let mut offset = COMMON_HEADER_LEN;
        while offset < input.len() {
            if input.len() - offset < CHUNK_HEADER_LEN {
                return Err(DataChannelError::MalformedChunk);
            }
            let length = usize::from(read_u16(input, offset + 2)?);
            if length < CHUNK_HEADER_LEN || length > input.len() - offset {
                return Err(DataChannelError::MalformedChunk);
            }
            chunks.push(ParsedChunk {
                kind: input[offset],
                flags: input[offset + 1],
                value: &input[offset + 4..offset + length],
            });
            offset = offset
                .checked_add((length + 3) & !3)
                .ok_or(DataChannelError::MalformedChunk)?;
            if offset > input.len() {
                return Err(DataChannelError::MalformedChunk);
            }
        }
        if chunks.is_empty() {
            return Err(DataChannelError::MalformedPacket);
        }
        Ok(Self {
            source_port: read_u16(input, 0)?,
            destination_port: read_u16(input, 2)?,
            verification_tag: read_u32(input, 4)?,
            chunks,
        })
    }
}

fn build_packet(
    source_port: u16,
    destination_port: u16,
    verification_tag: u32,
    kind: u8,
    flags: u8,
    value: &[u8],
) -> Result<Vec<u8>, DataChannelError> {
    let chunk_length = CHUNK_HEADER_LEN
        .checked_add(value.len())
        .ok_or(DataChannelError::MessageTooLarge)?;
    let chunk_length_u16 =
        u16::try_from(chunk_length).map_err(|_| DataChannelError::MessageTooLarge)?;
    let padded_length = (chunk_length + 3) & !3;
    let packet_length = COMMON_HEADER_LEN
        .checked_add(padded_length)
        .ok_or(DataChannelError::MessageTooLarge)?;
    if packet_length > 65_535 {
        return Err(DataChannelError::MessageTooLarge);
    }
    let mut packet = vec![0_u8; packet_length];
    packet[0..2].copy_from_slice(&source_port.to_be_bytes());
    packet[2..4].copy_from_slice(&destination_port.to_be_bytes());
    packet[4..8].copy_from_slice(&verification_tag.to_be_bytes());
    packet[12] = kind;
    packet[13] = flags;
    packet[14..16].copy_from_slice(&chunk_length_u16.to_be_bytes());
    packet[16..16 + value.len()].copy_from_slice(value);
    let checksum = checksum(&packet);
    packet[8..12].copy_from_slice(&checksum.to_le_bytes());
    Ok(packet)
}

fn push_parameter(output: &mut Vec<u8>, kind: u16, value: &[u8]) -> Result<(), DataChannelError> {
    let length = 4usize
        .checked_add(value.len())
        .ok_or(DataChannelError::MessageTooLarge)?;
    output.extend_from_slice(&kind.to_be_bytes());
    output.extend_from_slice(
        &u16::try_from(length)
            .map_err(|_| DataChannelError::MessageTooLarge)?
            .to_be_bytes(),
    );
    output.extend_from_slice(value);
    output.resize(output.len() + ((4 - length % 4) % 4), 0);
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct Parameter<'a> {
    kind: u16,
    value: &'a [u8],
}

fn parse_parameters(input: &[u8]) -> Result<Vec<Parameter<'_>>, DataChannelError> {
    let mut offset = 0;
    let mut parameters = Vec::with_capacity(2);
    while offset < input.len() {
        if input.len().saturating_sub(offset) < 4 {
            return Err(DataChannelError::MalformedChunk);
        }
        let kind = read_u16(input, offset)?;
        let length = usize::from(read_u16(input, offset + 2)?);
        if length < 4 || offset.saturating_add(length) > input.len() {
            return Err(DataChannelError::MalformedChunk);
        }
        parameters.push(Parameter {
            kind,
            value: &input[offset + 4..offset + length],
        });
        if offset + length == input.len() {
            offset = input.len();
            break;
        }
        offset = offset
            .checked_add((length + 3) & !3)
            .ok_or(DataChannelError::MalformedChunk)?;
    }
    if offset != input.len() {
        return Err(DataChannelError::MalformedChunk);
    }
    Ok(parameters)
}

fn parse_stream_numbers(
    input: &[u8],
    stream_limit: u16,
) -> Result<Option<Vec<u16>>, DataChannelError> {
    if input.is_empty() {
        return Ok(None);
    }
    if !input.len().is_multiple_of(2) {
        return Err(DataChannelError::MalformedChunk);
    }
    let streams = input
        .chunks_exact(2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    if streams.iter().any(|stream| *stream >= stream_limit) {
        return Err(DataChannelError::MalformedChunk);
    }
    Ok(Some(streams))
}

fn checksum(input: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for (index, supplied) in input.iter().copied().enumerate() {
        let byte = if (8..12).contains(&index) {
            0
        } else {
            supplied
        };
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82f6_3b78 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn read_u16(input: &[u8], offset: usize) -> Result<u16, DataChannelError> {
    let bytes = input
        .get(offset..offset.saturating_add(2))
        .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
        .ok_or(DataChannelError::MalformedPacket)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_u32(input: &[u8], offset: usize) -> Result<u32, DataChannelError> {
    let bytes = input
        .get(offset..offset.saturating_add(4))
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .ok_or(DataChannelError::MalformedPacket)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u32_le(input: &[u8], offset: usize) -> Result<u32, DataChannelError> {
    let bytes = input
        .get(offset..offset.saturating_add(4))
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .ok_or(DataChannelError::MalformedPacket)?;
    Ok(u32::from_le_bytes(bytes))
}

fn sequence_less_or_equal(candidate: u32, reference: u32) -> bool {
    reference.wrapping_sub(candidate) < (1 << 31)
}

fn sequence_less_or_equal_u16(candidate: u16, reference: u16) -> bool {
    reference.wrapping_sub(candidate) < (1 << 15)
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

/// SCTP/DCEP parse, state, or bounded-resource error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataChannelError {
    /// Association configuration is unsafe or internally inconsistent.
    InvalidConfiguration,
    /// Common header or packet length is invalid.
    MalformedPacket,
    /// CRC32C integrity check failed.
    InvalidChecksum,
    /// SCTP ports do not match SDP negotiation.
    UnexpectedPort,
    /// Chunk length or required fields are invalid.
    MalformedChunk,
    /// Packet verification tag does not belong to the association.
    InvalidVerificationTag,
    /// Chunk is invalid in the current association phase.
    UnexpectedState,
    /// COOKIE ECHO did not contain the issued state cookie.
    InvalidCookie,
    /// Receive TSN gap exceeded the bounded reorder window.
    ReceiveWindowExceeded,
    /// Fragment flags, stream sequence, or PPID changed within a message.
    InvalidFragmentSequence,
    /// DCEP OPEN is malformed, duplicated, excessive, or violates DTLS role parity.
    InvalidDcep,
    /// UTF-8 text or DCEP metadata is invalid.
    InvalidUtf8,
    /// User message exceeds the negotiated local limit.
    MessageTooLarge,
    /// Peer used an unnegotiated WebRTC PPID.
    UnsupportedPpid(u32),
    /// User DATA references an unopened stream.
    UnknownChannel,
    /// Application tried to send before cookie establishment.
    AssociationNotEstablished,
    /// Reliable outbound TSN window is full.
    SendWindowFull,
}

impl fmt::Display for DataChannelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for DataChannelError {}

#[cfg(test)]
mod tests {
    use super::{
        Association, AssociationConfig, AssociationEvent, AssociationState, ChannelReliability,
        DCEP_OPEN, DataChannelError, MAX_OUTSTANDING, MAX_RETRANSMISSIONS, MessageKind,
        PARAMETER_FORWARD_TSN_SUPPORTED, PARAMETER_INCOMING_RESET, PARAMETER_OUTGOING_RESET,
        PPID_BINARY, PPID_DCEP, ParsedPacket, TYPE_COOKIE_ECHO, TYPE_DATA, TYPE_FORWARD_TSN,
        TYPE_INIT, TYPE_RE_CONFIG, TYPE_SACK, build_packet, checksum, parse_parameters,
        push_parameter, read_u32,
    };
    use std::time::Duration;

    fn association() -> Association {
        Association::new(AssociationConfig {
            local_port: 5_000,
            remote_port: 5_000,
            verification_tag: 0x1122_3344,
            initial_tsn: 1_000,
            cookie: vec![0x55; 32],
            maximum_channels: 16,
            maximum_message_bytes: 16_384,
        })
        .expect("association")
    }

    fn peer_packet(tag: u32, kind: u8, flags: u8, value: &[u8]) -> Vec<u8> {
        build_packet(5_000, 5_000, tag, kind, flags, value).expect("packet")
    }

    fn establish(association: &mut Association) -> u32 {
        establish_with_partial_reliability(association, false)
    }

    fn establish_with_partial_reliability(
        association: &mut Association,
        partial_reliability: bool,
    ) -> u32 {
        let peer_tag = 0xaabb_ccdd_u32;
        let mut init = Vec::new();
        init.extend_from_slice(&peer_tag.to_be_bytes());
        init.extend_from_slice(&65_535_u32.to_be_bytes());
        init.extend_from_slice(&16_u16.to_be_bytes());
        init.extend_from_slice(&16_u16.to_be_bytes());
        init.extend_from_slice(&500_u32.to_be_bytes());
        if partial_reliability {
            push_parameter(&mut init, PARAMETER_FORWARD_TSN_SUPPORTED, &[])
                .expect("Forward TSN parameter");
        }
        let init_output = association
            .handle_packet(Duration::ZERO, &peer_packet(0, TYPE_INIT, 0, &init))
            .expect("INIT");
        assert_eq!(init_output.packets.len(), 1);
        assert_eq!(association.state(), AssociationState::CookieWait);
        let cookie = vec![0x55; 32];
        let output = association
            .handle_packet(
                Duration::from_millis(1),
                &peer_packet(0x1122_3344, TYPE_COOKIE_ECHO, 0, &cookie),
            )
            .expect("COOKIE ECHO");
        assert_eq!(output.events, vec![AssociationEvent::Established]);
        assert_eq!(association.state(), AssociationState::Established);
        peer_tag
    }

    fn sack(cumulative_tsn: u32) -> Vec<u8> {
        let mut value = Vec::new();
        value.extend_from_slice(&cumulative_tsn.to_be_bytes());
        value.extend_from_slice(&65_535_u32.to_be_bytes());
        value.extend_from_slice(&0_u16.to_be_bytes());
        value.extend_from_slice(&0_u16.to_be_bytes());
        value
    }

    fn sack_with_gap(cumulative_tsn: u32, start: u16, end: u16) -> Vec<u8> {
        let mut value = sack(cumulative_tsn);
        value[8..10].copy_from_slice(&1_u16.to_be_bytes());
        value.extend_from_slice(&start.to_be_bytes());
        value.extend_from_slice(&end.to_be_bytes());
        value
    }

    fn data_value(tsn: u32, stream: u16, sequence: u16, ppid: u32, payload: &[u8]) -> Vec<u8> {
        let mut value = Vec::new();
        value.extend_from_slice(&tsn.to_be_bytes());
        value.extend_from_slice(&stream.to_be_bytes());
        value.extend_from_slice(&sequence.to_be_bytes());
        value.extend_from_slice(&ppid.to_be_bytes());
        value.extend_from_slice(payload);
        value
    }

    #[test]
    fn crc32c_detects_tampering() {
        let mut packet = peer_packet(0, TYPE_INIT, 0, &[0; 16]);
        assert_eq!(
            u32::from_le_bytes(packet[8..12].try_into().expect("checksum")),
            checksum(&packet)
        );
        packet[20] ^= 1;
        assert_ne!(
            u32::from_le_bytes(packet[8..12].try_into().expect("checksum")),
            checksum(&packet)
        );
    }

    #[test]
    fn parses_an_unpadded_final_init_parameter() {
        let input = [0x80, 0x08, 0x00, 0x06, TYPE_FORWARD_TSN, TYPE_RE_CONFIG];
        let parameters = parse_parameters(&input).expect("final parameter");
        assert_eq!(parameters.len(), 1);
        assert_eq!(parameters[0].kind, 0x8008);
        assert_eq!(parameters[0].value, &[TYPE_FORWARD_TSN, TYPE_RE_CONFIG]);
    }

    #[test]
    fn opens_dcep_channel_and_round_trips_binary_message() {
        let mut association = association();
        let _peer_tag = establish(&mut association);
        let mut open = vec![DCEP_OPEN, 0x00];
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.extend_from_slice(&0_u32.to_be_bytes());
        open.extend_from_slice(&4_u16.to_be_bytes());
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.extend_from_slice(b"room");
        let output = association
            .handle_packet(
                Duration::from_millis(2),
                &peer_packet(
                    0x1122_3344,
                    TYPE_DATA,
                    0x03,
                    &data_value(500, 0, 0, PPID_DCEP, &open),
                ),
            )
            .expect("DCEP");
        assert!(matches!(
            output.events.first(),
            Some(AssociationEvent::ChannelOpened {
                stream_id: 0,
                label,
                ..
            }) if label == "room"
        ));
        assert_eq!(output.packets.len(), 2);

        let output = association
            .handle_packet(
                Duration::from_millis(3),
                &peer_packet(
                    0x1122_3344,
                    TYPE_DATA,
                    0x03,
                    &data_value(501, 0, 1, PPID_BINARY, b"hello"),
                ),
            )
            .expect("binary");
        assert_eq!(
            output.events,
            vec![AssociationEvent::Message {
                stream_id: 0,
                kind: MessageKind::Binary,
                payload: b"hello".to_vec(),
            }]
        );
        assert_eq!(
            association
                .send_message(Duration::from_millis(4), 0, MessageKind::Binary, b"reply")
                .expect("outbound")
                .len(),
            1
        );
    }

    #[test]
    fn reorders_tsn_and_reassembles_fragments() {
        let mut association = association();
        establish(&mut association);
        let mut open = vec![DCEP_OPEN, 0x00];
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.extend_from_slice(&0_u32.to_be_bytes());
        open.extend_from_slice(&1_u16.to_be_bytes());
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.push(b'x');
        association
            .handle_packet(
                Duration::from_millis(1),
                &peer_packet(
                    0x1122_3344,
                    TYPE_DATA,
                    0x03,
                    &data_value(500, 0, 0, PPID_DCEP, &open),
                ),
            )
            .expect("open");
        let end = association
            .handle_packet(
                Duration::from_millis(2),
                &peer_packet(
                    0x1122_3344,
                    TYPE_DATA,
                    0x01,
                    &data_value(502, 0, 1, PPID_BINARY, b"world"),
                ),
            )
            .expect("future end");
        assert!(end.events.is_empty());
        let begin = association
            .handle_packet(
                Duration::from_millis(3),
                &peer_packet(
                    0x1122_3344,
                    TYPE_DATA,
                    0x02,
                    &data_value(501, 0, 1, PPID_BINARY, b"hello "),
                ),
            )
            .expect("begin");
        assert_eq!(
            begin.events,
            vec![AssociationEvent::Message {
                stream_id: 0,
                kind: MessageKind::Binary,
                payload: b"hello world".to_vec(),
            }]
        );
    }

    #[test]
    fn retransmits_under_loss_and_fails_after_the_bounded_retry_budget() {
        let mut association = association();
        establish(&mut association);
        let mut open = vec![DCEP_OPEN, 0x00];
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.extend_from_slice(&0_u32.to_be_bytes());
        open.extend_from_slice(&1_u16.to_be_bytes());
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.push(b'x');
        association
            .handle_packet(
                Duration::from_millis(1),
                &peer_packet(
                    0x1122_3344,
                    TYPE_DATA,
                    0x03,
                    &data_value(500, 0, 0, PPID_DCEP, &open),
                ),
            )
            .expect("open");

        for retry in 1..=MAX_RETRANSMISSIONS {
            let output = association.tick(Duration::from_millis(1 + u64::from(retry) * 500));
            assert_eq!(output.packets.len(), 1);
            assert!(output.events.is_empty());
            assert_eq!(association.state(), AssociationState::Established);
        }
        let exhausted = association.tick(Duration::from_millis(
            1 + (u64::from(MAX_RETRANSMISSIONS) + 1) * 500,
        ));
        assert!(exhausted.packets.is_empty());
        assert!(matches!(
            exhausted.events.as_slice(),
            [AssociationEvent::DeliveryFailed { .. }]
        ));
        assert_eq!(association.state(), AssociationState::Failed);
    }

    #[test]
    fn rejects_partial_reliability_until_pr_sctp_is_negotiated() {
        let mut association = association();
        establish(&mut association);
        let mut open = vec![DCEP_OPEN, 0x01];
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.extend_from_slice(&1_u32.to_be_bytes());
        open.extend_from_slice(&1_u16.to_be_bytes());
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.push(b'x');
        assert!(
            association
                .handle_packet(
                    Duration::from_millis(1),
                    &peer_packet(
                        0x1122_3344,
                        TYPE_DATA,
                        0x03,
                        &data_value(500, 0, 0, PPID_DCEP, &open),
                    ),
                )
                .is_err()
        );
    }

    #[test]
    fn abandons_a_retry_limited_message_and_sends_forward_tsn() {
        let mut association = association();
        establish_with_partial_reliability(&mut association, true);
        let mut open = vec![DCEP_OPEN, 0x01];
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.extend_from_slice(&0_u32.to_be_bytes());
        open.extend_from_slice(&1_u16.to_be_bytes());
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.push(b'x');
        let opened = association
            .handle_packet(
                Duration::from_millis(1),
                &peer_packet(
                    0x1122_3344,
                    TYPE_DATA,
                    0x03,
                    &data_value(500, 0, 0, PPID_DCEP, &open),
                ),
            )
            .expect("partial DCEP open");
        assert!(matches!(
            opened.events.as_slice(),
            [AssociationEvent::ChannelOpened {
                reliability: ChannelReliability::MaxRetransmissions(0),
                ..
            }]
        ));
        association
            .handle_packet(
                Duration::from_millis(2),
                &peer_packet(0x1122_3344, TYPE_SACK, 0, &sack(1_000)),
            )
            .expect("ack DCEP response");
        association
            .send_message(Duration::from_millis(3), 0, MessageKind::Binary, b"drop me")
            .expect("partial message");

        let output = association.tick(Duration::from_millis(503));
        assert_eq!(
            output.events,
            vec![AssociationEvent::MessageAbandoned {
                stream_id: 0,
                stream_sequence: 0,
            }]
        );
        assert_eq!(association.state(), AssociationState::Established);
        assert_eq!(output.packets.len(), 1);
        let packet = ParsedPacket::parse(&output.packets[0]).expect("FORWARD TSN packet");
        assert_eq!(packet.chunks[0].kind, TYPE_FORWARD_TSN);
        assert_eq!(read_u32(packet.chunks[0].value, 0), Ok(1_001));
        assert_eq!(&packet.chunks[0].value[4..], &[0, 0, 0, 0]);

        assert!(
            association
                .tick(Duration::from_millis(1_002))
                .packets
                .is_empty(),
            "FORWARD TSN must respect the retransmission timer"
        );
        let retransmitted = association.tick(Duration::from_millis(1_003));
        assert_eq!(retransmitted.packets, output.packets);
        association
            .handle_packet(
                Duration::from_millis(1_004),
                &peer_packet(0x1122_3344, TYPE_SACK, 0, &sack(1_001)),
            )
            .expect("ack FORWARD TSN");
        assert!(
            association
                .tick(Duration::from_millis(1_504))
                .packets
                .is_empty(),
            "acknowledged FORWARD TSN must stop retransmitting"
        );
    }

    #[test]
    fn gap_ack_cannot_bypass_the_bounded_send_window() {
        let mut association = association();
        establish(&mut association);
        let mut open = vec![DCEP_OPEN, 0x00];
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.extend_from_slice(&0_u32.to_be_bytes());
        open.extend_from_slice(&1_u16.to_be_bytes());
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.push(b'x');
        association
            .handle_packet(
                Duration::from_millis(1),
                &peer_packet(
                    0x1122_3344,
                    TYPE_DATA,
                    0x03,
                    &data_value(500, 0, 0, PPID_DCEP, &open),
                ),
            )
            .expect("open");
        association
            .handle_packet(
                Duration::from_millis(2),
                &peer_packet(0x1122_3344, TYPE_SACK, 0, &sack(1_000)),
            )
            .expect("ack DCEP response");

        for index in 0..MAX_OUTSTANDING {
            association
                .send_message(
                    Duration::from_millis(3),
                    0,
                    MessageKind::Binary,
                    &[u8::try_from(index % 256).expect("bounded byte")],
                )
                .expect("bounded DATA");
        }
        association
            .handle_packet(
                Duration::from_millis(4),
                &peer_packet(
                    0x1122_3344,
                    TYPE_SACK,
                    0,
                    &sack_with_gap(
                        1_000,
                        1,
                        u16::try_from(MAX_OUTSTANDING).expect("bounded gap"),
                    ),
                ),
            )
            .expect("gap SACK");
        assert!(association.outstanding.is_empty());
        assert_eq!(
            association.send_message(
                Duration::from_millis(5),
                0,
                MessageKind::Binary,
                b"must wait for cumulative SACK",
            ),
            Err(DataChannelError::SendWindowFull)
        );
    }

    #[test]
    fn abandons_a_timed_message_at_its_negotiated_lifetime() {
        let mut association = association();
        establish_with_partial_reliability(&mut association, true);
        let mut open = vec![DCEP_OPEN, 0x82];
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.extend_from_slice(&100_u32.to_be_bytes());
        open.extend_from_slice(&1_u16.to_be_bytes());
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.push(b'x');
        association
            .handle_packet(
                Duration::from_millis(1),
                &peer_packet(
                    0x1122_3344,
                    TYPE_DATA,
                    0x03,
                    &data_value(500, 0, 0, PPID_DCEP, &open),
                ),
            )
            .expect("timed DCEP open");
        association
            .handle_packet(
                Duration::from_millis(2),
                &peer_packet(0x1122_3344, TYPE_SACK, 0, &sack(1_000)),
            )
            .expect("ack DCEP response");
        association
            .send_message(Duration::from_millis(10), 0, MessageKind::Binary, b"stale")
            .expect("timed message");
        assert!(
            association
                .tick(Duration::from_millis(109))
                .events
                .is_empty()
        );
        let output = association.tick(Duration::from_millis(110));
        assert!(matches!(
            output.events.as_slice(),
            [AssociationEvent::MessageAbandoned { stream_id: 0, .. }]
        ));
        assert_eq!(output.packets.len(), 1);
    }

    #[test]
    fn receives_forward_tsn_and_releases_buffered_data() {
        let mut association = association();
        establish_with_partial_reliability(&mut association, true);
        let mut open = vec![DCEP_OPEN, 0x01];
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.extend_from_slice(&0_u32.to_be_bytes());
        open.extend_from_slice(&1_u16.to_be_bytes());
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.push(b'x');
        association
            .handle_packet(
                Duration::from_millis(1),
                &peer_packet(
                    0x1122_3344,
                    TYPE_DATA,
                    0x03,
                    &data_value(500, 0, 0, PPID_DCEP, &open),
                ),
            )
            .expect("partial DCEP open");
        let buffered = association
            .handle_packet(
                Duration::from_millis(2),
                &peer_packet(
                    0x1122_3344,
                    TYPE_DATA,
                    0x03,
                    &data_value(502, 0, 1, PPID_BINARY, b"after gap"),
                ),
            )
            .expect("future DATA");
        assert!(buffered.events.is_empty());
        let mut forward = Vec::new();
        forward.extend_from_slice(&501_u32.to_be_bytes());
        forward.extend_from_slice(&0_u16.to_be_bytes());
        forward.extend_from_slice(&0_u16.to_be_bytes());
        let released = association
            .handle_packet(
                Duration::from_millis(3),
                &peer_packet(0x1122_3344, TYPE_FORWARD_TSN, 0, &forward),
            )
            .expect("FORWARD TSN");
        assert_eq!(
            released.events,
            vec![AssociationEvent::Message {
                stream_id: 0,
                kind: MessageKind::Binary,
                payload: b"after gap".to_vec(),
            }]
        );
    }

    #[test]
    fn closes_and_releases_a_dcep_stream_after_bidirectional_reset() {
        let mut association = association();
        establish(&mut association);
        let mut open = vec![DCEP_OPEN, 0x00];
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.extend_from_slice(&0_u32.to_be_bytes());
        open.extend_from_slice(&4_u16.to_be_bytes());
        open.extend_from_slice(&0_u16.to_be_bytes());
        open.extend_from_slice(b"room");
        association
            .handle_packet(
                Duration::from_millis(1),
                &peer_packet(
                    0x1122_3344,
                    TYPE_DATA,
                    0x03,
                    &data_value(500, 0, 0, PPID_DCEP, &open),
                ),
            )
            .expect("open");

        let mut reset = Vec::new();
        let mut outgoing = Vec::new();
        outgoing.extend_from_slice(&500_u32.to_be_bytes());
        outgoing.extend_from_slice(&999_u32.to_be_bytes());
        outgoing.extend_from_slice(&500_u32.to_be_bytes());
        outgoing.extend_from_slice(&0_u16.to_be_bytes());
        push_parameter(&mut reset, PARAMETER_OUTGOING_RESET, &outgoing).expect("outgoing reset");
        let mut incoming = Vec::new();
        incoming.extend_from_slice(&501_u32.to_be_bytes());
        incoming.extend_from_slice(&0_u16.to_be_bytes());
        push_parameter(&mut reset, PARAMETER_INCOMING_RESET, &incoming).expect("incoming reset");

        let output = association
            .handle_packet(
                Duration::from_millis(2),
                &peer_packet(0x1122_3344, TYPE_RE_CONFIG, 0, &reset),
            )
            .expect("stream reset");
        assert_eq!(
            output.events,
            vec![AssociationEvent::ChannelClosed { stream_id: 0 }]
        );
        assert_eq!(output.packets.len(), 1);
        assert!(
            association
                .send_message(Duration::from_millis(3), 0, MessageKind::Binary, b"closed")
                .is_err()
        );
    }
}
