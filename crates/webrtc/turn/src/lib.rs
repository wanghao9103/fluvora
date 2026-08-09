//! Bounded, Sans-I/O TURN allocation and `ChannelData` primitives.
//!
//! The network runtime lives in `fluvora-turn-server`; this crate owns the protocol state
//! invariants so allocation, permission, and channel behavior can be tested deterministically.

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use base64::Engine as _;
use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// Default TURN allocation lifetime.
pub const DEFAULT_ALLOCATION_LIFETIME: Duration = Duration::from_mins(10);
/// Maximum lifetime accepted from a client.
pub const MAX_ALLOCATION_LIFETIME: Duration = Duration::from_hours(1);
/// Permission lifetime fixed by TURN.
pub const PERMISSION_LIFETIME: Duration = Duration::from_mins(5);
/// Channel binding lifetime fixed by TURN.
pub const CHANNEL_LIFETIME: Duration = Duration::from_mins(10);
/// First valid TURN channel number.
pub const MIN_CHANNEL_NUMBER: u16 = 0x4000;
/// Last valid TURN channel number.
pub const MAX_CHANNEL_NUMBER: u16 = 0x4fff;
/// Maximum permissions retained by one allocation.
pub const MAX_PERMISSIONS: usize = 64;
/// Maximum channel bindings retained by one allocation.
pub const MAX_CHANNELS: usize = 64;

/// Derives the RFC long-term credential key used by MESSAGE-INTEGRITY (SHA-1).
#[must_use]
pub fn long_term_key(username: &str, realm: &str, password: &str) -> [u8; 16] {
    let mut digest = Md5::new();
    digest.update(username.as_bytes());
    digest.update(b":");
    digest.update(realm.as_bytes());
    digest.update(b":");
    digest.update(password.as_bytes());
    digest.finalize().into()
}

/// Produces the shared-secret password used by time-limited TURN REST credentials.
///
/// # Errors
///
/// Rejects a weak secret or an empty/oversized username.
pub fn rest_credential_password(secret: &[u8], username: &str) -> Result<String, TurnError> {
    if secret.len() < 32 || username.is_empty() || username.len() > 512 {
        return Err(TurnError::InvalidCredential);
    }
    let mut mac = HmacSha1::new_from_slice(secret).map_err(|_| TurnError::InvalidCredential)?;
    mac.update(username.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
}

/// One UDP TURN `ChannelData` message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelData<'a> {
    /// Bound channel number.
    pub channel_number: u16,
    /// Unencrypted application payload (normally DTLS/SRTP/STUN).
    pub data: &'a [u8],
}

impl<'a> ChannelData<'a> {
    /// Parses a UDP `ChannelData` datagram.
    ///
    /// # Errors
    ///
    /// Rejects out-of-range channel numbers, truncation, trailing non-padding bytes, and
    /// oversized payloads.
    pub fn parse(datagram: &'a [u8]) -> Result<Self, TurnError> {
        let header = datagram.get(..4).ok_or(TurnError::TruncatedChannelData)?;
        let channel_number = u16::from_be_bytes([header[0], header[1]]);
        if !(MIN_CHANNEL_NUMBER..=MAX_CHANNEL_NUMBER).contains(&channel_number) {
            return Err(TurnError::InvalidChannelNumber(channel_number));
        }
        let length = usize::from(u16::from_be_bytes([header[2], header[3]]));
        let end = 4usize
            .checked_add(length)
            .ok_or(TurnError::TruncatedChannelData)?;
        let data = datagram
            .get(4..end)
            .ok_or(TurnError::TruncatedChannelData)?;
        let padding = datagram.get(end..).ok_or(TurnError::TruncatedChannelData)?;
        if padding.len() > 3 || padding.iter().any(|byte| *byte != 0) {
            return Err(TurnError::TrailingChannelData);
        }
        Ok(Self {
            channel_number,
            data,
        })
    }

    /// Builds an unpadded UDP `ChannelData` datagram.
    ///
    /// # Errors
    ///
    /// Rejects invalid channel numbers or payloads larger than 65,535 bytes.
    pub fn encode(channel_number: u16, data: &[u8]) -> Result<Vec<u8>, TurnError> {
        if !(MIN_CHANNEL_NUMBER..=MAX_CHANNEL_NUMBER).contains(&channel_number) {
            return Err(TurnError::InvalidChannelNumber(channel_number));
        }
        let length = u16::try_from(data.len()).map_err(|_| TurnError::DataTooLarge)?;
        let mut output = Vec::with_capacity(4 + data.len());
        output.extend_from_slice(&channel_number.to_be_bytes());
        output.extend_from_slice(&length.to_be_bytes());
        output.extend_from_slice(data);
        Ok(output)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChannelBinding {
    peer: SocketAddr,
    expires_at: Duration,
}

/// One deterministic UDP relay allocation.
#[derive(Debug, Clone)]
pub struct Allocation {
    /// Authenticated TURN username.
    pub username: String,
    /// Client server-reflexive address.
    pub client_address: SocketAddr,
    /// Public address peers send to.
    pub relayed_address: SocketAddr,
    expires_at: Duration,
    permissions: HashMap<IpAddr, Duration>,
    channels: HashMap<u16, ChannelBinding>,
}

impl Allocation {
    /// Creates an allocation with the default ten-minute lifetime.
    #[must_use]
    pub fn new(
        now: Duration,
        username: impl Into<String>,
        client_address: SocketAddr,
        relayed_address: SocketAddr,
    ) -> Self {
        Self {
            username: username.into(),
            client_address,
            relayed_address,
            expires_at: now.saturating_add(DEFAULT_ALLOCATION_LIFETIME),
            permissions: HashMap::new(),
            channels: HashMap::new(),
        }
    }

    /// Returns seconds remaining in the allocation.
    #[must_use]
    pub fn remaining_lifetime(&self, now: Duration) -> u32 {
        u32::try_from(self.expires_at.saturating_sub(now).as_secs()).unwrap_or(u32::MAX)
    }

    /// Returns whether the allocation expired.
    #[must_use]
    pub fn expired(&self, now: Duration) -> bool {
        now >= self.expires_at
    }

    /// Refreshes the allocation and returns the granted lifetime.
    pub fn refresh(&mut self, now: Duration, requested_seconds: Option<u32>) -> u32 {
        let requested = requested_seconds
            .map_or(DEFAULT_ALLOCATION_LIFETIME, |seconds| {
                Duration::from_secs(u64::from(seconds))
            })
            .min(MAX_ALLOCATION_LIFETIME);
        self.expires_at = now.saturating_add(requested);
        u32::try_from(requested.as_secs()).unwrap_or(u32::MAX)
    }

    /// Installs or refreshes IP permissions atomically.
    ///
    /// # Errors
    ///
    /// Rejects mixed address families or capacity overflow.
    pub fn create_permissions(
        &mut self,
        now: Duration,
        peers: &[SocketAddr],
    ) -> Result<(), TurnError> {
        self.expire_children(now);
        if peers.is_empty()
            || peers
                .iter()
                .any(|peer| !same_family(peer.ip(), self.relayed_address.ip()))
        {
            return Err(TurnError::PeerAddressFamilyMismatch);
        }
        let additional = peers
            .iter()
            .map(SocketAddr::ip)
            .filter(|ip| !self.permissions.contains_key(ip))
            .collect::<std::collections::HashSet<_>>()
            .len();
        if self.permissions.len().saturating_add(additional) > MAX_PERMISSIONS {
            return Err(TurnError::Capacity);
        }
        let expires_at = now.saturating_add(PERMISSION_LIFETIME);
        for peer in peers {
            self.permissions.insert(peer.ip(), expires_at);
        }
        Ok(())
    }

    /// Creates or refreshes a channel binding and its permission.
    ///
    /// # Errors
    ///
    /// Enforces channel range, allocation family, uniqueness, and capacity.
    pub fn bind_channel(
        &mut self,
        now: Duration,
        channel_number: u16,
        peer: SocketAddr,
    ) -> Result<(), TurnError> {
        self.expire_children(now);
        if !(MIN_CHANNEL_NUMBER..=MAX_CHANNEL_NUMBER).contains(&channel_number) {
            return Err(TurnError::InvalidChannelNumber(channel_number));
        }
        if !same_family(peer.ip(), self.relayed_address.ip()) {
            return Err(TurnError::PeerAddressFamilyMismatch);
        }
        if self
            .channels
            .get(&channel_number)
            .is_some_and(|binding| binding.peer != peer)
            || self
                .channels
                .iter()
                .any(|(number, binding)| *number != channel_number && binding.peer == peer)
        {
            return Err(TurnError::ChannelConflict);
        }
        if !self.channels.contains_key(&channel_number) && self.channels.len() >= MAX_CHANNELS {
            return Err(TurnError::Capacity);
        }
        self.create_permissions(now, &[peer])?;
        self.channels.insert(
            channel_number,
            ChannelBinding {
                peer,
                expires_at: now.saturating_add(CHANNEL_LIFETIME),
            },
        );
        Ok(())
    }

    /// Resolves client `ChannelData` into a permitted peer destination.
    #[must_use]
    pub fn channel_peer(&mut self, now: Duration, channel_number: u16) -> Option<SocketAddr> {
        self.expire_children(now);
        let binding = self.channels.get(&channel_number)?;
        self.permissions
            .contains_key(&binding.peer.ip())
            .then_some(binding.peer)
    }

    /// Checks whether a Send indication may target a peer.
    #[must_use]
    pub fn permits(&mut self, now: Duration, peer: SocketAddr) -> bool {
        self.expire_children(now);
        self.permissions.contains_key(&peer.ip())
            && same_family(peer.ip(), self.relayed_address.ip())
    }

    /// Chooses `ChannelData` when a live binding exists; otherwise chooses a Data indication.
    #[must_use]
    pub fn peer_route(&mut self, now: Duration, peer: SocketAddr) -> Option<PeerRoute> {
        if !self.permits(now, peer) {
            return None;
        }
        self.channels
            .iter()
            .find_map(|(number, binding)| {
                (binding.peer == peer).then_some(PeerRoute::Channel(*number))
            })
            .or(Some(PeerRoute::DataIndication))
    }

    fn expire_children(&mut self, now: Duration) {
        self.permissions.retain(|_, expires_at| *expires_at > now);
        self.channels.retain(|_, binding| binding.expires_at > now);
    }
}

/// Encapsulation used for a datagram received from a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerRoute {
    /// Compact `ChannelData` frame.
    Channel(u16),
    /// STUN Data indication carrying XOR-PEER-ADDRESS and DATA.
    DataIndication,
}

const fn same_family(left: IpAddr, right: IpAddr) -> bool {
    matches!(
        (left, right),
        (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
    )
}

/// TURN state/codec failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnError {
    /// `ChannelData` header or body is truncated.
    TruncatedChannelData,
    /// `ChannelData` contains invalid trailing bytes.
    TrailingChannelData,
    /// Channel is outside 0x4000..=0x4fff.
    InvalidChannelNumber(u16),
    /// Application data exceeds the 16-bit `ChannelData` length.
    DataTooLarge,
    /// Peer and relay address families differ.
    PeerAddressFamilyMismatch,
    /// A channel number or peer is already bound differently.
    ChannelConflict,
    /// Per-allocation resource bound was reached.
    Capacity,
    /// TURN REST shared secret or username is invalid.
    InvalidCredential,
}

impl fmt::Display for TurnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for TurnError {}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Duration;

    use super::{
        Allocation, ChannelData, PeerRoute, TurnError, long_term_key, rest_credential_password,
    };

    fn address(last: u8, port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::new(192, 0, 2, last), port))
    }

    #[test]
    fn derives_known_long_term_key() {
        assert_eq!(
            long_term_key("user", "example.org", "pass"),
            [
                0xab, 0xca, 0x35, 0x35, 0x6f, 0x4b, 0x00, 0xfb, 0xc3, 0x3e, 0x2d, 0x8c, 0x2c, 0x43,
                0xb9, 0xd6
            ]
        );
    }

    #[test]
    fn derives_deterministic_rest_credential() {
        let secret = [3_u8; 32];
        let password = rest_credential_password(&secret, "2000000000:user").expect("credential");
        assert_eq!(password, "13uRc0dabWJm28vlnmsHO/f0KDQ=");
        assert!(rest_credential_password(b"weak", "user").is_err());
    }

    #[test]
    fn channel_data_round_trips_and_rejects_trailing_bytes() {
        let encoded = ChannelData::encode(0x4000, b"media").expect("encode");
        let decoded = ChannelData::parse(&encoded).expect("decode");
        assert_eq!(decoded.channel_number, 0x4000);
        assert_eq!(decoded.data, b"media");
        let mut invalid = encoded;
        invalid.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(
            ChannelData::parse(&invalid),
            Err(TurnError::TrailingChannelData)
        );
    }

    #[test]
    fn allocation_enforces_permissions_channels_and_expiry() {
        let now = Duration::from_secs(1);
        let peer = address(20, 50_000);
        let mut allocation = Allocation::new(now, "user", address(10, 40_000), address(1, 60_000));
        assert!(!allocation.permits(now, peer));
        allocation.bind_channel(now, 0x4000, peer).expect("channel");
        assert_eq!(allocation.channel_peer(now, 0x4000), Some(peer));
        assert_eq!(
            allocation.peer_route(now, peer),
            Some(PeerRoute::Channel(0x4000))
        );
        assert!(!allocation.permits(now + Duration::from_secs(301), peer));
    }
}
