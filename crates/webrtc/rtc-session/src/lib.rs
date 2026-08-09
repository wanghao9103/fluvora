//! Composite Sans-I/O WebRTC transport session.

use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use fluvora_dtls_adapter::DirectionalKeyingMaterial;
use fluvora_ice_lite::{
    Agent as IceAgent, Event as IceEvent, IceError, IceState, Transmit as IceTransmit,
};
use fluvora_rtc_datagram::{DatagramKind, classify};
use fluvora_rtcp::{Packet as RtcpPacket, parse_compound};
use fluvora_rtp::Packet as RtpPacket;
use fluvora_srtp::{SrtpContext, SrtpError};

/// Composite transport state exposed to room/session monitoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Awaiting an authenticated ICE connectivity check.
    New,
    /// ICE nomination succeeded and DTLS records are expected.
    DtlsHandshaking,
    /// DTLS fingerprint was verified and SRTP keys are installed.
    Connected,
    /// ICE consent has temporarily expired.
    Disconnected,
    /// ICE failed; an ICE restart is required.
    Failed,
    /// Application permanently closed the session.
    Closed,
}

/// UDP transmission requested by the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transmit {
    /// Local source address.
    pub source: SocketAddr,
    /// Authenticated ICE destination address.
    pub destination: SocketAddr,
    /// Datagram bytes.
    pub payload: Vec<u8>,
}

/// Side effect emitted by datagram or timer processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAction {
    /// Runtime should send this datagram.
    Transmit(Transmit),
    /// Authenticated-pair DTLS record for the configured crypto backend.
    DtlsInput(Vec<u8>),
    /// Authenticated and decrypted RTP packet.
    InboundRtp(Vec<u8>),
    /// Authenticated, decrypted, and decoded RTCP compound packets.
    InboundRtcp {
        /// Clear SRTCP plaintext.
        bytes: Vec<u8>,
        /// Decoded control packets.
        packets: Vec<RtcpPacket>,
    },
    /// Public composite state changed.
    StateChanged {
        /// State before the transition.
        from: SessionState,
        /// State after the transition.
        to: SessionState,
    },
}

/// One ICE generation, selected tuple, and SRTP context.
#[derive(Debug)]
pub struct Session {
    ice: IceAgent,
    state: SessionState,
    selected_local: Option<SocketAddr>,
    selected_remote: Option<SocketAddr>,
    srtp: Option<SrtpContext>,
}

impl Session {
    /// Creates a session around a fresh ICE-lite agent.
    #[must_use]
    pub const fn new(ice: IceAgent) -> Self {
        Self {
            ice,
            state: SessionState::New,
            selected_local: None,
            selected_remote: None,
            srtp: None,
        }
    }

    /// Returns the composite state.
    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.state
    }

    /// Returns the nominated destination.
    #[must_use]
    pub const fn selected_remote(&self) -> Option<SocketAddr> {
        self.selected_remote
    }

    /// Routes one UDP datagram through ICE, DTLS, or SRTP.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] for malformed/authentication-failed traffic, tuple spoofing,
    /// encrypted media before DTLS completion, or traffic after close.
    pub fn handle_datagram(
        &mut self,
        now: Duration,
        local: SocketAddr,
        remote: SocketAddr,
        input: &[u8],
    ) -> Result<Vec<SessionAction>, SessionError> {
        if self.state == SessionState::Closed {
            return Err(SessionError::Closed);
        }
        match classify(input) {
            DatagramKind::Stun => self.handle_stun(now, local, remote, input),
            DatagramKind::Dtls => {
                self.require_selected_tuple(local, remote)?;
                if !matches!(
                    self.state,
                    SessionState::DtlsHandshaking | SessionState::Connected
                ) {
                    return Err(SessionError::UnexpectedDatagram(DatagramKind::Dtls));
                }
                Ok(vec![SessionAction::DtlsInput(input.to_vec())])
            }
            DatagramKind::Rtp => {
                self.require_selected_tuple(local, remote)?;
                let plaintext = self
                    .srtp
                    .as_mut()
                    .ok_or(SessionError::SrtpNotReady)?
                    .unprotect_rtp(input)?;
                RtpPacket::parse(&plaintext)?;
                Ok(vec![SessionAction::InboundRtp(plaintext)])
            }
            DatagramKind::Rtcp => {
                self.require_selected_tuple(local, remote)?;
                let plaintext = self
                    .srtp
                    .as_mut()
                    .ok_or(SessionError::SrtpNotReady)?
                    .unprotect_rtcp(input)?;
                let packets = parse_compound(&plaintext)?;
                Ok(vec![SessionAction::InboundRtcp {
                    bytes: plaintext,
                    packets,
                }])
            }
            kind => Err(SessionError::UnexpectedDatagram(kind)),
        }
    }

    /// Applies ICE consent timers.
    #[must_use]
    pub fn tick(&mut self, now: Duration) -> Vec<SessionAction> {
        let mut actions = Vec::new();
        for event in self.ice.tick(now) {
            self.apply_ice_event(&event, &mut actions);
        }
        actions
    }

    /// Installs SRTP keys only after the DTLS backend has verified the SDP certificate fingerprint.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] unless ICE nomination put the session in DTLS handshaking state.
    pub fn install_dtls_keying_material(
        &mut self,
        keying: &DirectionalKeyingMaterial,
    ) -> Result<Vec<SessionAction>, SessionError> {
        if self.state != SessionState::DtlsHandshaking {
            return Err(SessionError::InvalidState(self.state));
        }
        self.srtp = Some(SrtpContext::new(
            keying.profile,
            &keying.outbound,
            &keying.inbound,
        ));
        Ok(self
            .transition(SessionState::Connected)
            .into_iter()
            .collect())
    }

    /// Wraps an outbound clear RTP packet for the selected pair.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] if SRTP is not ready, RTP is malformed, or no pair is selected.
    pub fn protect_rtp(&mut self, mut packet: Vec<u8>) -> Result<Transmit, SessionError> {
        let srtp = self.srtp.as_mut().ok_or(SessionError::SrtpNotReady)?;
        srtp.protect_rtp(&mut packet)?;
        self.selected_transmit(packet)
    }

    /// Wraps an outbound clear RTCP compound packet for the selected pair.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] if SRTCP is not ready, RTCP is malformed, or no pair is selected.
    pub fn protect_rtcp(&mut self, mut packet: Vec<u8>) -> Result<Transmit, SessionError> {
        let srtp = self.srtp.as_mut().ok_or(SessionError::SrtpNotReady)?;
        srtp.protect_rtcp(&mut packet)?;
        self.selected_transmit(packet)
    }

    /// Wraps a DTLS backend output record for the selected pair.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when no pair has been nominated.
    pub fn transmit_dtls(&self, payload: Vec<u8>) -> Result<Transmit, SessionError> {
        self.selected_transmit(payload)
    }

    /// Permanently closes the transport and erases the SRTP context.
    #[must_use]
    pub fn close(&mut self) -> Vec<SessionAction> {
        self.srtp = None;
        self.transition(SessionState::Closed).into_iter().collect()
    }

    /// Starts a fresh ICE generation while retaining fingerprint-verified DTLS/SRTP keys.
    ///
    /// A newly nominated tuple can therefore resume protected media without a second DTLS
    /// handshake when the peer certificate and transport remain unchanged.
    pub fn restart_ice(&mut self, configuration: fluvora_ice_lite::Configuration) {
        self.ice.restart(configuration);
        self.selected_local = None;
        self.selected_remote = None;
        self.state = SessionState::New;
    }

    fn handle_stun(
        &mut self,
        now: Duration,
        local: SocketAddr,
        remote: SocketAddr,
        input: &[u8],
    ) -> Result<Vec<SessionAction>, SessionError> {
        let output = self.ice.handle_datagram(now, local, remote, input)?;
        let mut actions = vec![SessionAction::Transmit(map_ice_transmit(output.transmit))];
        for event in output.events {
            self.apply_ice_event(&event, &mut actions);
        }
        Ok(actions)
    }

    fn apply_ice_event(&mut self, event: &IceEvent, actions: &mut Vec<SessionAction>) {
        match event {
            IceEvent::SelectedPair(pair) => {
                self.selected_local = Some(pair.local);
                self.selected_remote = Some(pair.remote);
            }
            IceEvent::StateChanged { to, .. } => {
                let target = match to {
                    IceState::New => SessionState::New,
                    IceState::Completed
                        if matches!(self.state, SessionState::New | SessionState::Disconnected) =>
                    {
                        if self.srtp.is_some() {
                            SessionState::Connected
                        } else {
                            SessionState::DtlsHandshaking
                        }
                    }
                    IceState::Connected | IceState::Completed => self.state,
                    IceState::Disconnected => SessionState::Disconnected,
                    IceState::Failed => SessionState::Failed,
                };
                actions.extend(self.transition(target));
            }
        }
    }

    fn require_selected_tuple(
        &self,
        local: SocketAddr,
        remote: SocketAddr,
    ) -> Result<(), SessionError> {
        if self.selected_local == Some(local) && self.selected_remote == Some(remote) {
            Ok(())
        } else {
            Err(SessionError::TupleMismatch)
        }
    }

    fn selected_transmit(&self, payload: Vec<u8>) -> Result<Transmit, SessionError> {
        Ok(Transmit {
            source: self.selected_local.ok_or(SessionError::NoSelectedPair)?,
            destination: self.selected_remote.ok_or(SessionError::NoSelectedPair)?,
            payload,
        })
    }

    fn transition(&mut self, target: SessionState) -> Option<SessionAction> {
        if target == self.state {
            return None;
        }
        let from = self.state;
        self.state = target;
        Some(SessionAction::StateChanged { from, to: target })
    }
}

fn map_ice_transmit(transmit: IceTransmit) -> Transmit {
    Transmit {
        source: transmit.source,
        destination: transmit.destination,
        payload: transmit.payload,
    }
}

/// Composite session processing failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// ICE rejected the datagram.
    Ice(IceError),
    /// SRTP rejected or could not encode the packet.
    Srtp(SrtpError),
    /// RTP plaintext was malformed.
    Rtp(fluvora_rtp::RtpError),
    /// RTCP plaintext was malformed.
    Rtcp(fluvora_rtcp::RtcpError),
    /// Datagram arrived in a state that does not accept its protocol.
    UnexpectedDatagram(DatagramKind),
    /// Datagram source/destination does not match the nominated ICE pair.
    TupleMismatch,
    /// DTLS has not installed SRTP keys.
    SrtpNotReady,
    /// No nominated pair exists for output.
    NoSelectedPair,
    /// Operation is not legal in the current state.
    InvalidState(SessionState),
    /// Session is permanently closed.
    Closed,
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ice(error) => error.fmt(formatter),
            Self::Srtp(error) => error.fmt(formatter),
            Self::Rtp(error) => error.fmt(formatter),
            Self::Rtcp(error) => error.fmt(formatter),
            Self::UnexpectedDatagram(kind) => write!(formatter, "unexpected {kind:?} datagram"),
            Self::TupleMismatch => {
                formatter.write_str("datagram does not match selected ICE tuple")
            }
            Self::SrtpNotReady => formatter.write_str("SRTP keys are not installed"),
            Self::NoSelectedPair => formatter.write_str("ICE has no selected pair"),
            Self::InvalidState(state) => write!(formatter, "invalid RTC session state {state:?}"),
            Self::Closed => formatter.write_str("RTC session is closed"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<IceError> for SessionError {
    fn from(value: IceError) -> Self {
        Self::Ice(value)
    }
}

impl From<SrtpError> for SessionError {
    fn from(value: SrtpError) -> Self {
        Self::Srtp(value)
    }
}

impl From<fluvora_rtp::RtpError> for SessionError {
    fn from(value: fluvora_rtp::RtpError) -> Self {
        Self::Rtp(value)
    }
}

impl From<fluvora_rtcp::RtcpError> for SessionError {
    fn from(value: fluvora_rtcp::RtcpError) -> Self {
        Self::Rtcp(value)
    }
}
