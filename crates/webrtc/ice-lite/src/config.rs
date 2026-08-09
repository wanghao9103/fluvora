use std::fmt;
use std::time::Duration;

const MIN_PASSWORD_BYTES: usize = 22;
const MAX_CREDENTIAL_BYTES: usize = 256;

/// One ICE username fragment and short-term password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    username_fragment: String,
    password: Vec<u8>,
}

impl Credentials {
    /// Creates validated ICE credentials.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] when the username fragment or password violates RFC 8445
    /// length requirements, or when the username contains a colon.
    pub fn new(
        username_fragment: impl Into<String>,
        password: impl Into<Vec<u8>>,
    ) -> Result<Self, CredentialError> {
        let username_fragment = username_fragment.into();
        let password = password.into();
        if username_fragment.is_empty() || username_fragment.len() > MAX_CREDENTIAL_BYTES {
            return Err(CredentialError::InvalidUsernameFragmentLength(
                username_fragment.len(),
            ));
        }
        if username_fragment.contains(':') {
            return Err(CredentialError::UsernameFragmentContainsColon);
        }
        if !(MIN_PASSWORD_BYTES..=MAX_CREDENTIAL_BYTES).contains(&password.len()) {
            return Err(CredentialError::InvalidPasswordLength(password.len()));
        }
        Ok(Self {
            username_fragment,
            password,
        })
    }

    /// Returns the username fragment.
    #[must_use]
    pub fn username_fragment(&self) -> &str {
        &self.username_fragment
    }

    /// Returns the short-term password bytes.
    #[must_use]
    pub fn password(&self) -> &[u8] {
        &self.password
    }
}

/// ICE credential validation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    /// The username fragment is empty or longer than 256 bytes.
    InvalidUsernameFragmentLength(usize),
    /// A colon would make the inbound ICE username ambiguous.
    UsernameFragmentContainsColon,
    /// The password is shorter than 22 or longer than 256 bytes.
    InvalidPasswordLength(usize),
}

impl fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUsernameFragmentLength(length) => {
                write!(formatter, "invalid ICE username fragment length: {length}")
            }
            Self::UsernameFragmentContainsColon => {
                formatter.write_str("ICE username fragment contains ':'")
            }
            Self::InvalidPasswordLength(length) => {
                write!(formatter, "invalid ICE password length: {length}")
            }
        }
    }
}

impl std::error::Error for CredentialError {}

/// Runtime limits and credentials for one ICE generation.
#[derive(Debug, Clone)]
pub struct Configuration {
    /// Credentials advertised by this ICE-lite agent.
    pub local_credentials: Credentials,
    /// Credentials received from the full ICE peer.
    pub remote_credentials: Credentials,
    /// Local 64-bit tie breaker used in role-conflict responses.
    pub tie_breaker: u64,
    /// Time without an authenticated check before the transport becomes disconnected.
    pub consent_timeout: Duration,
    /// Time without an authenticated check before the transport fails.
    pub failure_timeout: Duration,
    /// Maximum number of peer-reflexive candidate pairs retained.
    pub max_candidate_pairs: usize,
}

impl Configuration {
    /// Creates a configuration with WebRTC-oriented timeout and resource defaults.
    #[must_use]
    pub const fn new(
        local_credentials: Credentials,
        remote_credentials: Credentials,
        tie_breaker: u64,
    ) -> Self {
        Self {
            local_credentials,
            remote_credentials,
            tie_breaker,
            consent_timeout: Duration::from_secs(30),
            failure_timeout: Duration::from_mins(1),
            max_candidate_pairs: 64,
        }
    }
}
