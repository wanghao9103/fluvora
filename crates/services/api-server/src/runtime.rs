use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;

use crate::error::{ApiError, internal_error};

pub(crate) const EVENT_TICKET_TTL_MILLIS: u64 = 30_000;
pub(crate) const MAX_PROTOCOL_SESSIONS: usize = 100_000;

pub(crate) fn format_id(value: u128) -> String {
    format!("{value:032x}")
}

pub(crate) fn random_u128() -> Result<u128, ApiError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(internal_error)?;
    Ok(u128::from_be_bytes(bytes))
}

pub(crate) fn random_u64() -> Result<u64, ApiError> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(internal_error)?;
    Ok(u64::from_be_bytes(bytes))
}

pub(crate) fn random_sdp_session_id() -> Result<u64, ApiError> {
    Ok(normalize_sdp_session_id(random_u64()?))
}

const fn normalize_sdp_session_id(random: u64) -> u64 {
    let session_id = random & 0x7fff_ffff_ffff_ffff;
    if session_id == 0 { 1 } else { session_id }
}

pub(crate) fn random_credential(bytes: usize) -> Result<String, ApiError> {
    let mut random = vec![0_u8; bytes];
    getrandom::fill(&mut random).map_err(internal_error)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random))
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_sdp_session_id;

    #[test]
    fn sdp_session_identifiers_are_nonzero_signed_63_bit_values() {
        for random in [0, 1, i64::MAX as u64, u64::MAX] {
            let session_id = normalize_sdp_session_id(random);
            assert!((1..=i64::MAX as u64).contains(&session_id));
        }
    }
}
