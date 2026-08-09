//! Shared-UDP WebRTC session routing for the media-node runtime.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use fluvora_ice_lite::{Agent, Configuration, Credentials};
use fluvora_observability::MediaNodeMetrics;
use fluvora_rtc_datagram::{DatagramKind, classify};
use fluvora_rtc_session::{Session, SessionAction, SessionState, Transmit};
use fluvora_stun::Message;

mod sfu_runtime;

pub use sfu_runtime::{PublishTrack, SfuRegistry, SfuRoute, SfuRuntimeError, SubscribeTrack};

/// Hard resource limit for one process.
pub const MAX_SESSIONS: usize = 100_000;

/// Validated control-plane input used to allocate one ICE generation.
#[derive(Debug, Clone)]
pub struct SessionProvision {
    /// Control-plane generated stable identifier.
    pub session_id: String,
    /// Room identifier retained for authorization/diagnostics.
    pub room_id: String,
    /// Authenticated participant identifier.
    pub participant_id: String,
    /// ICE username fragment advertised by Fluvora.
    pub local_username_fragment: String,
    /// ICE password advertised by Fluvora.
    pub local_password: String,
    /// Browser/SDK ICE username fragment from the offer.
    pub remote_username_fragment: String,
    /// Browser/SDK ICE password from the offer.
    pub remote_password: String,
    /// Browser/SDK SDP certificate fingerprint.
    pub expected_peer_fingerprint: String,
    /// ICE role conflict tie breaker.
    pub tie_breaker: u64,
}

/// Fresh credentials for an in-place ICE generation restart.
#[derive(Debug, Clone)]
pub struct SessionIceRestart {
    /// Stable existing session identifier.
    pub session_id: String,
    /// New media-node username fragment.
    pub local_username_fragment: String,
    /// New media-node password.
    pub local_password: String,
    /// New peer username fragment.
    pub remote_username_fragment: String,
    /// New peer password.
    pub remote_password: String,
    /// Fresh role-conflict tie breaker.
    pub tie_breaker: u64,
}

#[derive(Debug)]
struct ManagedSession {
    room_id: String,
    participant_id: String,
    expected_peer_fingerprint: String,
    session: Mutex<Session>,
}

/// Concurrent routing table shared by UDP and control HTTP tasks.
#[derive(Debug)]
pub struct SessionRegistry {
    sessions: RwLock<HashMap<String, Arc<ManagedSession>>>,
    local_ufrag_to_session: RwLock<HashMap<String, String>>,
    remote_to_session: RwLock<HashMap<SocketAddr, String>>,
    metrics: Arc<MediaNodeMetrics>,
    capacity: usize,
}

impl SessionRegistry {
    /// Creates a bounded empty registry.
    #[must_use]
    pub fn new(metrics: Arc<MediaNodeMetrics>, capacity: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            local_ufrag_to_session: RwLock::new(HashMap::new()),
            remote_to_session: RwLock::new(HashMap::new()),
            metrics,
            capacity: capacity.min(MAX_SESSIONS),
        }
    }

    /// Provisions an ICE/RTC session before the SDP answer is returned.
    ///
    /// # Errors
    ///
    /// Rejects malformed metadata, invalid ICE credentials, duplicates, and capacity exhaustion.
    pub fn provision(&self, input: SessionProvision) -> Result<(), RegistryError> {
        validate_identifier(&input.session_id, 128)?;
        validate_identifier(&input.room_id, 128)?;
        validate_identifier(&input.participant_id, 128)?;
        if input.expected_peer_fingerprint.is_empty() || input.expected_peer_fingerprint.len() > 256
        {
            return Err(RegistryError::InvalidFingerprint);
        }
        let local_credentials =
            Credentials::new(input.local_username_fragment.clone(), input.local_password)
                .map_err(|error| RegistryError::InvalidIceCredentials(error.to_string()))?;
        let remote_credentials =
            Credentials::new(input.remote_username_fragment, input.remote_password)
                .map_err(|error| RegistryError::InvalidIceCredentials(error.to_string()))?;
        let session = Session::new(Agent::new(Configuration::new(
            local_credentials,
            remote_credentials,
            input.tie_breaker,
        )));
        let mut sessions = self
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut ufrags = self
            .local_ufrag_to_session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if sessions.contains_key(&input.session_id) {
            return Err(RegistryError::DuplicateSession);
        }
        if ufrags.contains_key(&input.local_username_fragment) {
            return Err(RegistryError::DuplicateUsernameFragment);
        }
        if sessions.len() >= self.capacity {
            return Err(RegistryError::Capacity);
        }
        ufrags.insert(input.local_username_fragment, input.session_id.clone());
        sessions.insert(
            input.session_id,
            Arc::new(ManagedSession {
                room_id: input.room_id,
                participant_id: input.participant_id,
                expected_peer_fingerprint: input.expected_peer_fingerprint,
                session: Mutex::new(session),
            }),
        );
        self.metrics.active_sessions.add(1);
        Ok(())
    }

    /// Replaces the ICE generation for an existing session while preserving DTLS/SRTP state.
    ///
    /// # Errors
    ///
    /// Rejects missing sessions, invalid credentials, or a local username fragment already owned
    /// by another session.
    pub fn restart_ice(&self, input: SessionIceRestart) -> Result<(), RegistryError> {
        validate_identifier(&input.session_id, 128)?;
        let local_username_fragment = input.local_username_fragment.clone();
        let local_credentials =
            Credentials::new(local_username_fragment.clone(), input.local_password)
                .map_err(|error| RegistryError::InvalidIceCredentials(error.to_string()))?;
        let remote_credentials =
            Credentials::new(input.remote_username_fragment, input.remote_password)
                .map_err(|error| RegistryError::InvalidIceCredentials(error.to_string()))?;
        let managed = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&input.session_id)
            .cloned()
            .ok_or(RegistryError::UnknownSession)?;
        let mut ufrags = self
            .local_ufrag_to_session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if ufrags
            .get(&local_username_fragment)
            .is_some_and(|owner| owner != &input.session_id)
        {
            return Err(RegistryError::DuplicateUsernameFragment);
        }
        managed
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .restart_ice(Configuration::new(
                local_credentials,
                remote_credentials,
                input.tie_breaker,
            ));
        ufrags.retain(|_, owner| owner != &input.session_id);
        ufrags.insert(local_username_fragment, input.session_id.clone());
        self.remote_to_session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, owner| owner != &input.session_id);
        Ok(())
    }

    /// Removes a session and all route aliases.
    #[must_use]
    pub fn remove(&self, session_id: &str) -> bool {
        let removed = self
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
        if removed.is_none() {
            return false;
        }
        self.local_ufrag_to_session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, mapped| mapped != session_id);
        self.remote_to_session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, mapped| mapped != session_id);
        self.metrics.active_sessions.add(-1);
        true
    }

    /// Routes one datagram to a session and returns its runtime actions.
    ///
    /// # Errors
    ///
    /// Returns a structured error for unknown routes, malformed STUN, or transport rejection.
    pub fn handle_datagram(
        &self,
        now: Duration,
        local: SocketAddr,
        remote: SocketAddr,
        bytes: &[u8],
    ) -> Result<RoutedActions, RegistryError> {
        let session_id = match classify(bytes) {
            DatagramKind::Stun => self.route_stun(bytes)?,
            _ => self
                .remote_to_session
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&remote)
                .cloned()
                .ok_or(RegistryError::UnknownRemote)?,
        };
        let managed = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .cloned()
            .ok_or(RegistryError::UnknownSession)?;
        let mut session = managed
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let actions = session
            .handle_datagram(now, local, remote, bytes)
            .map_err(|error| RegistryError::Transport(error.to_string()))?;
        if session.selected_remote() == Some(remote) {
            let mut routes = self
                .remote_to_session
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if routes
                .get(&remote)
                .is_some_and(|existing| existing != &session_id)
            {
                return Err(RegistryError::RemoteCollision);
            }
            routes.insert(remote, session_id.clone());
        }
        if actions.iter().any(|action| {
            matches!(
                action,
                SessionAction::InboundRtp(_) | SessionAction::InboundRtcp { .. }
            )
        }) {
            self.metrics
                .media_bytes_received
                .add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        }
        if actions
            .iter()
            .any(|action| matches!(action, SessionAction::InboundRtp(_)))
        {
            self.metrics.rtp_packets_received.increment();
        }
        Ok(RoutedActions {
            session_id,
            room_id: managed.room_id.clone(),
            participant_id: managed.participant_id.clone(),
            expected_peer_fingerprint: managed.expected_peer_fingerprint.clone(),
            actions,
        })
    }

    /// Applies consent timers to every session.
    #[must_use]
    pub fn tick(&self, now: Duration) -> Vec<RoutedActions> {
        let sessions: Vec<_> = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(id, session)| (id.clone(), Arc::clone(session)))
            .collect();
        sessions
            .into_iter()
            .filter_map(|(session_id, managed)| {
                let actions = managed
                    .session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .tick(now);
                (!actions.is_empty()).then(|| RoutedActions {
                    session_id,
                    room_id: managed.room_id.clone(),
                    participant_id: managed.participant_id.clone(),
                    expected_peer_fingerprint: managed.expected_peer_fingerprint.clone(),
                    actions,
                })
            })
            .collect()
    }

    /// Installs fingerprint-verified DTLS-SRTP exporter material.
    ///
    /// # Errors
    ///
    /// Rejects unknown sessions or key installation in an invalid transport state.
    pub fn install_dtls_keying_material(
        &self,
        session_id: &str,
        keying: &fluvora_dtls_adapter::DirectionalKeyingMaterial,
    ) -> Result<Vec<SessionAction>, RegistryError> {
        let managed = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
            .ok_or(RegistryError::UnknownSession)?;
        managed
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .install_dtls_keying_material(keying)
            .map_err(|error| RegistryError::Transport(error.to_string()))
    }

    /// Wraps a DTLS record for the authenticated selected tuple.
    ///
    /// # Errors
    ///
    /// Rejects unknown sessions or output before ICE nomination.
    pub fn transmit_dtls(
        &self,
        session_id: &str,
        payload: Vec<u8>,
    ) -> Result<Transmit, RegistryError> {
        let managed = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
            .ok_or(RegistryError::UnknownSession)?;
        managed
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .transmit_dtls(payload)
            .map_err(|error| RegistryError::Transport(error.to_string()))
    }

    /// Protects one clear SFU RTP output for a destination session.
    ///
    /// # Errors
    ///
    /// Rejects unknown or not-yet-connected sessions and malformed RTP.
    pub fn protect_rtp(
        &self,
        session_id: &str,
        packet: Vec<u8>,
    ) -> Result<Transmit, RegistryError> {
        let managed = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
            .ok_or(RegistryError::UnknownSession)?;
        managed
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .protect_rtp(packet)
            .map_err(|error| RegistryError::Transport(error.to_string()))
    }

    /// Protects one clear SFU RTCP output for a destination session.
    ///
    /// # Errors
    ///
    /// Rejects unknown or not-yet-connected sessions and malformed RTCP.
    pub fn protect_rtcp(
        &self,
        session_id: &str,
        packet: Vec<u8>,
    ) -> Result<Transmit, RegistryError> {
        let managed = self
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
            .ok_or(RegistryError::UnknownSession)?;
        managed
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .protect_rtcp(packet)
            .map_err(|error| RegistryError::Transport(error.to_string()))
    }

    /// Returns bounded public session state.
    #[must_use]
    pub fn session_snapshot(&self, session_id: &str) -> Option<SessionSnapshot> {
        self.sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .map(|managed| SessionSnapshot {
                session_id: session_id.to_owned(),
                room_id: managed.room_id.clone(),
                participant_id: managed.participant_id.clone(),
                state: managed
                    .session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .state(),
            })
    }

    /// Returns current allocated sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Returns whether no sessions are allocated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns bounded session identifiers currently assigned to a room.
    #[must_use]
    pub fn session_ids_in_room(&self, room_id: &str) -> Vec<String> {
        self.sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(|(session_id, managed)| {
                (managed.room_id == room_id).then_some(session_id.clone())
            })
            .collect()
    }

    fn route_stun(&self, bytes: &[u8]) -> Result<String, RegistryError> {
        let message =
            Message::parse(bytes).map_err(|error| RegistryError::Stun(error.to_string()))?;
        let username = message
            .username()
            .map_err(|error| RegistryError::Stun(error.to_string()))?
            .ok_or(RegistryError::MissingUsername)?;
        let local_ufrag = username
            .split_once(':')
            .map_or(username, |(local, _)| local);
        self.local_ufrag_to_session
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(local_ufrag)
            .cloned()
            .ok_or(RegistryError::UnknownUsernameFragment)
    }
}

/// Routed session output with the metadata needed by SFU and DTLS layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedActions {
    /// Session identifier.
    pub session_id: String,
    /// Owning room.
    pub room_id: String,
    /// Owning participant.
    pub participant_id: String,
    /// Authenticated signaling fingerprint expectation.
    pub expected_peer_fingerprint: String,
    /// Transport actions.
    pub actions: Vec<SessionAction>,
}

/// Public bounded state returned by the control API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    /// Session identifier.
    pub session_id: String,
    /// Room identifier.
    pub room_id: String,
    /// Participant identifier.
    pub participant_id: String,
    /// Composite transport state.
    pub state: SessionState,
}

/// Routing/provisioning error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// Public identifier is malformed.
    InvalidIdentifier,
    /// SDP fingerprint is empty or unreasonably large.
    InvalidFingerprint,
    /// ICE credentials fail RFC bounds.
    InvalidIceCredentials(String),
    /// Session identifier already exists.
    DuplicateSession,
    /// Local username fragment must be process-unique.
    DuplicateUsernameFragment,
    /// Bounded process capacity reached.
    Capacity,
    /// Datagram source has not completed STUN routing.
    UnknownRemote,
    /// Session disappeared during routing.
    UnknownSession,
    /// STUN packet is malformed.
    Stun(String),
    /// STUN request omitted USERNAME.
    MissingUsername,
    /// USERNAME does not address a provisioned generation.
    UnknownUsernameFragment,
    /// Two sessions attempted to claim the same remote tuple.
    RemoteCollision,
    /// ICE/SRTP/session state rejected traffic.
    Transport(String),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for RegistryError {}

fn validate_identifier(value: &str, maximum: usize) -> Result<(), RegistryError> {
    if value.is_empty()
        || value.len() > maximum
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
    {
        return Err(RegistryError::InvalidIdentifier);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::time::Duration;

    use fluvora_observability::MediaNodeMetrics;
    use fluvora_rtc_session::{SessionAction, SessionState};
    use fluvora_stun::{MessageBuilder, MessageClass, MessageType, Method, TransactionId};

    use super::{SessionIceRestart, SessionProvision, SessionRegistry};

    fn provision() -> SessionProvision {
        SessionProvision {
            session_id: "session-1".to_owned(),
            room_id: "room-1".to_owned(),
            participant_id: "user-1".to_owned(),
            local_username_fragment: "server".to_owned(),
            local_password: "server-password-1234567".to_owned(),
            remote_username_fragment: "client".to_owned(),
            remote_password: "client-password-1234567".to_owned(),
            expected_peer_fingerprint: "AA:BB".to_owned(),
            tie_breaker: 7,
        }
    }

    #[test]
    fn routes_authenticated_stun_then_pins_remote_tuple() {
        let metrics = Arc::new(MediaNodeMetrics::default());
        let registry = SessionRegistry::new(metrics, 2);
        registry.provision(provision()).expect("provision");
        let bytes = MessageBuilder::new(
            MessageType::new(Method::BINDING, MessageClass::Request),
            TransactionId::new([1; 12]),
        )
        .username("server:client")
        .priority(1_000)
        .ice_controlling(9)
        .use_candidate()
        .message_integrity_sha1(b"server-password-1234567".to_vec())
        .fingerprint()
        .build()
        .expect("stun");
        let local = SocketAddr::from((Ipv4Addr::LOCALHOST, 5_000));
        let remote = SocketAddr::from((Ipv4Addr::LOCALHOST, 40_000));
        let output = registry
            .handle_datagram(Duration::from_secs(1), local, remote, &bytes)
            .expect("route");
        assert_eq!(output.session_id, "session-1");
        assert!(matches!(
            output.actions.first(),
            Some(SessionAction::Transmit(_))
        ));
        assert_eq!(
            registry
                .session_snapshot("session-1")
                .expect("snapshot")
                .state,
            SessionState::DtlsHandshaking
        );
    }

    #[test]
    fn rejects_duplicates_and_cleans_aliases() {
        let registry = SessionRegistry::new(Arc::new(MediaNodeMetrics::default()), 1);
        registry.provision(provision()).expect("provision");
        assert!(registry.provision(provision()).is_err());
        assert!(registry.remove("session-1"));
        assert!(registry.is_empty());
        registry.provision(provision()).expect("reprovision");
    }

    #[test]
    fn restart_replaces_ice_generation_aliases() {
        let registry = SessionRegistry::new(Arc::new(MediaNodeMetrics::default()), 1);
        registry.provision(provision()).expect("provision");
        registry
            .restart_ice(SessionIceRestart {
                session_id: "session-1".to_owned(),
                local_username_fragment: "server2".to_owned(),
                local_password: "server-password-7654321".to_owned(),
                remote_username_fragment: "client2".to_owned(),
                remote_password: "client-password-7654321".to_owned(),
                tie_breaker: 8,
            })
            .expect("restart");
        let old = MessageBuilder::new(
            MessageType::new(Method::BINDING, MessageClass::Request),
            TransactionId::new([2; 12]),
        )
        .username("server:client")
        .priority(1_000)
        .ice_controlling(9)
        .use_candidate()
        .message_integrity_sha1(b"server-password-1234567".to_vec())
        .fingerprint()
        .build()
        .expect("old check");
        assert!(
            registry
                .handle_datagram(
                    Duration::from_secs(1),
                    SocketAddr::from((Ipv4Addr::LOCALHOST, 5_000)),
                    SocketAddr::from((Ipv4Addr::LOCALHOST, 40_000)),
                    &old,
                )
                .is_err()
        );
        let current = MessageBuilder::new(
            MessageType::new(Method::BINDING, MessageClass::Request),
            TransactionId::new([3; 12]),
        )
        .username("server2:client2")
        .priority(1_000)
        .ice_controlling(10)
        .use_candidate()
        .message_integrity_sha1(b"server-password-7654321".to_vec())
        .fingerprint()
        .build()
        .expect("new check");
        assert!(
            registry
                .handle_datagram(
                    Duration::from_secs(2),
                    SocketAddr::from((Ipv4Addr::LOCALHOST, 5_000)),
                    SocketAddr::from((Ipv4Addr::LOCALHOST, 40_001)),
                    &current,
                )
                .is_ok()
        );
    }
}
