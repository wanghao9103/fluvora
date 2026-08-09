use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use fluvora_stun::{
    AttributeType, Message, MessageBuilder, MessageClass, MessageType, Method, StunError,
    TransactionId,
};

use crate::Configuration;

/// Connectivity state for one ICE generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceState {
    /// No authenticated connectivity check has arrived.
    New,
    /// At least one valid pair exists, but no pair has been nominated.
    Connected,
    /// The controlling peer nominated a selected pair.
    Completed,
    /// Consent expired, but the failure timeout has not elapsed.
    Disconnected,
    /// The failure timeout elapsed; an ICE restart is required.
    Failed,
}

/// The integrity algorithm used by a validated connectivity check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityAlgorithm {
    /// MESSAGE-INTEGRITY with HMAC-SHA1, used by current WebRTC implementations.
    Sha1,
    /// MESSAGE-INTEGRITY-SHA256 with HMAC-SHA256.
    Sha256,
}

/// A validated local/remote candidate pair learned from a connectivity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidatePair {
    /// Address on which Fluvora received the check.
    pub local: SocketAddr,
    /// Source address authenticated by the check.
    pub remote: SocketAddr,
    /// Peer-reflexive candidate priority supplied by the full ICE agent.
    pub priority: u32,
    /// Whether the controlling peer nominated this pair.
    pub nominated: bool,
    /// Last monotonic timestamp at which the pair was authenticated.
    pub last_authenticated: Duration,
}

/// An application-observable ICE event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The public ICE state changed.
    StateChanged {
        /// State before the transition.
        from: IceState,
        /// State after the transition.
        to: IceState,
    },
    /// The controlling peer selected a pair using USE-CANDIDATE.
    SelectedPair(CandidatePair),
}

/// One UDP transmission for the embedding runtime to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transmit {
    /// Local address that should source the response.
    pub source: SocketAddr,
    /// Remote destination.
    pub destination: SocketAddr,
    /// Encoded STUN packet.
    pub payload: Vec<u8>,
}

/// Complete result of handling one datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleOutput {
    /// Binding success or authenticated error response.
    pub transmit: Transmit,
    /// State and pair-selection events caused by the request.
    pub events: Vec<Event>,
}

/// An ICE-lite agent for one peer connection and ICE generation.
#[derive(Debug, Clone)]
pub struct Agent {
    configuration: Configuration,
    state: IceState,
    pairs: Vec<CandidatePair>,
    selected_pair: Option<CandidatePair>,
    last_authenticated: Option<Duration>,
}

impl Agent {
    /// Creates a fresh ICE generation.
    #[must_use]
    pub const fn new(configuration: Configuration) -> Self {
        Self {
            configuration,
            state: IceState::New,
            pairs: Vec::new(),
            selected_pair: None,
            last_authenticated: None,
        }
    }

    /// Returns the current connectivity state.
    #[must_use]
    pub const fn state(&self) -> IceState {
        self.state
    }

    /// Returns all retained valid candidate pairs.
    #[must_use]
    pub fn candidate_pairs(&self) -> &[CandidatePair] {
        &self.pairs
    }

    /// Returns the nominated pair, if any.
    #[must_use]
    pub const fn selected_pair(&self) -> Option<&CandidatePair> {
        self.selected_pair.as_ref()
    }

    /// Handles one UDP datagram without performing I/O.
    ///
    /// Authentication failures are returned without a transmission to avoid turning the server
    /// into an unauthenticated reflection endpoint. Requests that pass authentication but violate
    /// ICE semantics receive signed STUN error responses.
    ///
    /// # Errors
    ///
    /// Returns [`IceError`] for malformed, unauthenticated, or non-ICE datagrams and for encoding
    /// failures.
    pub fn handle_datagram(
        &mut self,
        now: Duration,
        local: SocketAddr,
        remote: SocketAddr,
        input: &[u8],
    ) -> Result<HandleOutput, IceError> {
        if self.state == IceState::Failed {
            return Err(IceError::RestartRequired);
        }
        let message = Message::parse(input).map_err(IceError::Stun)?;
        validate_message_type(&message)?;
        let algorithm = self.authenticate(&message)?;

        if let Some(output) = self.semantic_error_response(&message, algorithm, local, remote)? {
            return Ok(output);
        }

        let priority = message
            .priority()
            .map_err(IceError::Stun)?
            .ok_or(IceError::MissingPriority)?;
        let nominated = message.use_candidate();
        let mut events = self.record_valid_pair(now, local, remote, priority, nominated)?;
        let payload = self.success_response(&message, remote, algorithm)?;
        events.extend(self.transition_after_check(nominated));

        Ok(HandleOutput {
            transmit: Transmit {
                source: local,
                destination: remote,
                payload,
            },
            events,
        })
    }

    /// Advances consent timers and returns state-change events.
    #[must_use]
    pub fn tick(&mut self, now: Duration) -> Vec<Event> {
        let Some(last_authenticated) = self.last_authenticated else {
            return Vec::new();
        };
        let elapsed = now.saturating_sub(last_authenticated);
        if elapsed >= self.configuration.failure_timeout {
            self.transition(IceState::Failed).into_iter().collect()
        } else if elapsed >= self.configuration.consent_timeout {
            self.transition(IceState::Disconnected)
                .into_iter()
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Replaces both credential generations and clears all transport state.
    pub fn restart(&mut self, configuration: Configuration) {
        self.configuration = configuration;
        self.state = IceState::New;
        self.pairs.clear();
        self.selected_pair = None;
        self.last_authenticated = None;
    }

    fn authenticate(&self, message: &Message<'_>) -> Result<IntegrityAlgorithm, IceError> {
        let expected_username = format!(
            "{}:{}",
            self.configuration.local_credentials.username_fragment(),
            self.configuration.remote_credentials.username_fragment()
        );
        let username = message.username().map_err(IceError::Stun)?;
        if username != Some(expected_username.as_str()) {
            return Err(IceError::InvalidUsername);
        }
        let algorithm = if message
            .attribute(AttributeType::MESSAGE_INTEGRITY_SHA256)
            .is_some()
        {
            message
                .verify_message_integrity_sha256(self.configuration.local_credentials.password())
                .map_err(IceError::Stun)?;
            IntegrityAlgorithm::Sha256
        } else {
            message
                .verify_message_integrity_sha1(self.configuration.local_credentials.password())
                .map_err(IceError::Stun)?;
            IntegrityAlgorithm::Sha1
        };
        if message.attribute(AttributeType::FINGERPRINT).is_some() {
            message.verify_fingerprint().map_err(IceError::Stun)?;
        }
        Ok(algorithm)
    }

    fn semantic_error_response(
        &self,
        message: &Message<'_>,
        algorithm: IntegrityAlgorithm,
        local: SocketAddr,
        remote: SocketAddr,
    ) -> Result<Option<HandleOutput>, IceError> {
        let unknown = message.unknown_required_attributes();
        if !unknown.is_empty() {
            let builder = Self::error_builder(message.transaction_id(), 420, "Unknown Attribute")?
                .unknown_attributes(&unknown);
            return self
                .error_output(builder, algorithm, local, remote)
                .map(Some);
        }
        let has_controlling = message.ice_controlling().map_err(IceError::Stun)?.is_some();
        let has_controlled = message.ice_controlled().map_err(IceError::Stun)?.is_some();
        if has_controlled || !has_controlling {
            let builder = Self::error_builder(message.transaction_id(), 487, "Role Conflict")?
                .ice_controlled(self.configuration.tie_breaker);
            return self
                .error_output(builder, algorithm, local, remote)
                .map(Some);
        }
        if message.priority().map_err(IceError::Stun)?.is_none() {
            let builder = Self::error_builder(message.transaction_id(), 400, "Bad Request")?;
            return self
                .error_output(builder, algorithm, local, remote)
                .map(Some);
        }
        Ok(None)
    }

    fn error_builder(
        transaction_id: TransactionId,
        code: u16,
        reason: &str,
    ) -> Result<MessageBuilder, IceError> {
        MessageBuilder::new(
            MessageType::new(Method::BINDING, MessageClass::ErrorResponse),
            transaction_id,
        )
        .error_code(code, reason)
        .map_err(IceError::Stun)
    }

    fn error_output(
        &self,
        builder: MessageBuilder,
        algorithm: IntegrityAlgorithm,
        local: SocketAddr,
        remote: SocketAddr,
    ) -> Result<HandleOutput, IceError> {
        let payload = add_integrity(
            builder,
            algorithm,
            self.configuration.local_credentials.password(),
        )
        .fingerprint()
        .build()
        .map_err(IceError::Stun)?;
        Ok(HandleOutput {
            transmit: Transmit {
                source: local,
                destination: remote,
                payload,
            },
            events: Vec::new(),
        })
    }

    fn success_response(
        &self,
        message: &Message<'_>,
        remote: SocketAddr,
        algorithm: IntegrityAlgorithm,
    ) -> Result<Vec<u8>, IceError> {
        let builder = MessageBuilder::new(
            MessageType::new(Method::BINDING, MessageClass::SuccessResponse),
            message.transaction_id(),
        )
        .xor_mapped_address(remote);
        add_integrity(
            builder,
            algorithm,
            self.configuration.local_credentials.password(),
        )
        .fingerprint()
        .build()
        .map_err(IceError::Stun)
    }

    fn record_valid_pair(
        &mut self,
        now: Duration,
        local: SocketAddr,
        remote: SocketAddr,
        priority: u32,
        nominated: bool,
    ) -> Result<Vec<Event>, IceError> {
        self.last_authenticated = Some(now);
        let pair = if let Some(pair) = self
            .pairs
            .iter_mut()
            .find(|pair| pair.local == local && pair.remote == remote)
        {
            pair.priority = priority;
            pair.nominated |= nominated;
            pair.last_authenticated = now;
            pair.clone()
        } else {
            if self.pairs.len() >= self.configuration.max_candidate_pairs {
                return Err(IceError::CandidatePairLimit);
            }
            let pair = CandidatePair {
                local,
                remote,
                priority,
                nominated,
                last_authenticated: now,
            };
            self.pairs.push(pair.clone());
            pair
        };

        if nominated && self.selected_pair.as_ref() != Some(&pair) {
            self.selected_pair = Some(pair.clone());
            Ok(vec![Event::SelectedPair(pair)])
        } else {
            Ok(Vec::new())
        }
    }

    fn transition_after_check(&mut self, nominated: bool) -> Vec<Event> {
        let target = if nominated || self.selected_pair.is_some() {
            IceState::Completed
        } else {
            IceState::Connected
        };
        self.transition(target).into_iter().collect()
    }

    fn transition(&mut self, target: IceState) -> Option<Event> {
        if self.state == target {
            return None;
        }
        let from = self.state;
        self.state = target;
        Some(Event::StateChanged { from, to: target })
    }
}

fn validate_message_type(message: &Message<'_>) -> Result<(), IceError> {
    let message_type = message.message_type();
    if message_type.method() == Method::BINDING && message_type.class() == MessageClass::Request {
        Ok(())
    } else {
        Err(IceError::NotBindingRequest)
    }
}

fn add_integrity(
    builder: MessageBuilder,
    algorithm: IntegrityAlgorithm,
    password: &[u8],
) -> MessageBuilder {
    match algorithm {
        IntegrityAlgorithm::Sha1 => builder.message_integrity_sha1(password.to_vec()),
        IntegrityAlgorithm::Sha256 => builder.message_integrity_sha256(password.to_vec()),
    }
}

/// Datagram validation or state-machine errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IceError {
    /// STUN parsing, authentication, or encoding failed.
    Stun(StunError),
    /// The datagram was not a Binding request.
    NotBindingRequest,
    /// USERNAME did not identify this local and remote ICE generation.
    InvalidUsername,
    /// PRIORITY was absent after semantic validation.
    MissingPriority,
    /// The configured candidate-pair resource limit was reached.
    CandidatePairLimit,
    /// The generation has failed and must be restarted with fresh credentials.
    RestartRequired,
}

impl fmt::Display for IceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stun(error) => error.fmt(formatter),
            Self::NotBindingRequest => {
                formatter.write_str("datagram is not a STUN Binding request")
            }
            Self::InvalidUsername => formatter.write_str("ICE USERNAME does not match generation"),
            Self::MissingPriority => formatter.write_str("ICE connectivity check lacks PRIORITY"),
            Self::CandidatePairLimit => formatter.write_str("ICE candidate pair limit reached"),
            Self::RestartRequired => formatter.write_str("ICE generation requires restart"),
        }
    }
}

impl std::error::Error for IceError {}

impl From<StunError> for IceError {
    fn from(value: StunError) -> Self {
        Self::Stun(value)
    }
}
