use std::net::IpAddr;
use std::time::Duration;

use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const TAG_BYTES: usize = 32;

#[derive(Debug, Clone)]
pub struct NonceManager {
    secret: Vec<u8>,
    lifetime: Duration,
}

impl NonceManager {
    pub fn new(secret: Vec<u8>, lifetime: Duration) -> Result<Self, NonceError> {
        if secret.len() < 32 || lifetime.is_zero() || lifetime > Duration::from_hours(1) {
            return Err(NonceError::InvalidConfiguration);
        }
        Ok(Self { secret, lifetime })
    }

    pub fn issue(&self, now_seconds: u64, client_ip: IpAddr) -> Result<String, NonceError> {
        let mut payload = nonce_payload(now_seconds, client_ip);
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .map_err(|_| NonceError::InvalidConfiguration)?;
        mac.update(&payload);
        payload.extend_from_slice(&mac.finalize().into_bytes());
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload))
    }

    pub fn validate(
        &self,
        nonce: &str,
        now_seconds: u64,
        client_ip: IpAddr,
    ) -> Result<(), NonceError> {
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(nonce)
            .map_err(|_| NonceError::Invalid)?;
        let payload_length = nonce_payload(0, client_ip).len();
        if decoded.len() != payload_length + TAG_BYTES {
            return Err(NonceError::Invalid);
        }
        let (payload, tag) = decoded.split_at(payload_length);
        let timestamp_bytes: [u8; 8] = payload
            .get(..8)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(NonceError::Invalid)?;
        let timestamp = u64::from_be_bytes(timestamp_bytes);
        if payload != nonce_payload(timestamp, client_ip) {
            return Err(NonceError::Invalid);
        }
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .map_err(|_| NonceError::InvalidConfiguration)?;
        mac.update(payload);
        mac.verify_slice(tag).map_err(|_| NonceError::Invalid)?;
        if timestamp > now_seconds
            || now_seconds.saturating_sub(timestamp) > self.lifetime.as_secs()
        {
            return Err(NonceError::Stale);
        }
        Ok(())
    }
}

fn nonce_payload(timestamp: u64, client_ip: IpAddr) -> Vec<u8> {
    let mut payload = Vec::with_capacity(25);
    payload.extend_from_slice(&timestamp.to_be_bytes());
    match client_ip {
        IpAddr::V4(address) => {
            payload.push(4);
            payload.extend_from_slice(&address.octets());
        }
        IpAddr::V6(address) => {
            payload.push(6);
            payload.extend_from_slice(&address.octets());
        }
    }
    payload
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonceError {
    InvalidConfiguration,
    Invalid,
    Stale,
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    use super::{NonceError, NonceManager};

    #[test]
    fn binds_nonce_to_ip_and_expiration() {
        let manager = NonceManager::new(vec![7; 32], Duration::from_mins(10)).expect("manager");
        let client = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let nonce = manager.issue(1_000, client).expect("nonce");
        assert_eq!(manager.validate(&nonce, 1_600, client), Ok(()));
        assert_eq!(
            manager.validate(&nonce, 1_601, client),
            Err(NonceError::Stale)
        );
        assert_eq!(
            manager.validate(&nonce, 1_001, IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2))),
            Err(NonceError::Invalid)
        );
    }
}
