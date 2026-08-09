use axum::http::StatusCode;
use base64::Engine as _;
use fluvora_domain::RoomId;
use hmac::{Hmac, Mac as _};
use serde::Deserialize;
use sha2::Sha256;

use crate::error::ApiError;

const RECEIPT_MAX_CLOCK_SKEW_MILLIS: u64 = 5 * 60 * 1_000;
const SHA256_BASE64URL_LENGTH: usize = 43;
const MAX_PROVIDER_LENGTH: usize = 64;
const MAX_TRANSACTION_ID_LENGTH: usize = 512;
const MAX_GIFT_ID_LENGTH: usize = 256;

#[derive(Debug, Deserialize)]
pub(super) struct GiftRequest {
    pub(super) provider: String,
    pub(super) provider_timestamp_millis: u64,
    provider_signature: String,
    pub(super) sender_id: String,
    pub(super) transaction_id: String,
    pub(super) gift_id: String,
    pub(super) quantity: u32,
    pub(super) unit_value: u64,
    pub(super) currency: String,
    pub(super) recipient_id: String,
}

pub(super) fn verify_gift_receipt(
    secret: &[u8],
    room_id: RoomId,
    request: &GiftRequest,
    now_millis: u64,
) -> Result<(), ApiError> {
    if !valid_receipt_shape(request, now_millis) {
        return Err(invalid_gift_signature());
    }

    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&request.provider_signature)
        .map_err(|_| invalid_gift_signature())?;
    gift_receipt_mac(secret, room_id, request)?
        .verify_slice(&signature)
        .map_err(|_| invalid_gift_signature())
}

fn valid_receipt_shape(request: &GiftRequest, now_millis: u64) -> bool {
    now_millis.abs_diff(request.provider_timestamp_millis) <= RECEIPT_MAX_CLOCK_SKEW_MILLIS
        && safe_identifier(&request.provider, MAX_PROVIDER_LENGTH)
        && bounded_text(&request.transaction_id, MAX_TRANSACTION_ID_LENGTH)
        && bounded_text(&request.gift_id, MAX_GIFT_ID_LENGTH)
        && valid_hex_id(&request.sender_id)
        && valid_hex_id(&request.recipient_id)
        && request.quantity > 0
        && request
            .unit_value
            .checked_mul(u64::from(request.quantity))
            .is_some()
        && request.currency.len() == 3
        && request
            .currency
            .bytes()
            .all(|byte| byte.is_ascii_uppercase())
        && request.provider_signature.len() == SHA256_BASE64URL_LENGTH
}

fn safe_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn bounded_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn valid_hex_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn gift_receipt_mac(
    secret: &[u8],
    room_id: RoomId,
    request: &GiftRequest,
) -> Result<Hmac<Sha256>, ApiError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| invalid_gift_signature())?;
    mac.update(b"fluvora-gift-receipt-v1");
    gift_mac_field(&mut mac, &format!("{:032x}", room_id.0));
    gift_mac_field(&mut mac, &request.provider);
    gift_mac_field(&mut mac, &request.transaction_id);
    gift_mac_field(&mut mac, &request.sender_id);
    gift_mac_field(&mut mac, &request.recipient_id);
    gift_mac_field(&mut mac, &request.gift_id);
    mac.update(&request.quantity.to_be_bytes());
    mac.update(&request.unit_value.to_be_bytes());
    gift_mac_field(&mut mac, &request.currency);
    mac.update(&request.provider_timestamp_millis.to_be_bytes());
    Ok(mac)
}

fn gift_mac_field(mac: &mut Hmac<Sha256>, value: &str) {
    mac.update(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    mac.update(value.as_bytes());
}

fn invalid_gift_signature() -> ApiError {
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "invalid_gift_receipt_signature",
        message: "gift receipt signature, timestamp, or fields are invalid".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use fluvora_domain::RoomId;
    use hmac::Mac as _;

    use super::{GiftRequest, gift_receipt_mac, verify_gift_receipt};

    const SECRET: &[u8] = b"gift-webhook-secret-that-is-long-enough";
    const NOW: u64 = 1_800_000_000_000;

    fn signed_request() -> GiftRequest {
        let mut request = GiftRequest {
            provider: "payment-provider".to_owned(),
            provider_timestamp_millis: NOW,
            provider_signature: String::new(),
            sender_id: "00000000000000000000000000000001".to_owned(),
            transaction_id: "payment-42".to_owned(),
            gift_id: "rocket".to_owned(),
            quantity: 2,
            unit_value: 500,
            currency: "CNY".to_owned(),
            recipient_id: "00000000000000000000000000000002".to_owned(),
        };
        request.provider_signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            gift_receipt_mac(SECRET, RoomId(0x1234), &request)
                .expect("gift MAC")
                .finalize()
                .into_bytes(),
        );
        request
    }

    #[test]
    fn verifies_fresh_receipts_and_rejects_tampering() {
        let mut request = signed_request();
        assert!(verify_gift_receipt(SECRET, RoomId(0x1234), &request, NOW).is_ok());

        request.quantity = 3;
        assert!(verify_gift_receipt(SECRET, RoomId(0x1234), &request, NOW).is_err());
        request.quantity = 2;
        assert!(verify_gift_receipt(SECRET, RoomId(0x1234), &request, NOW + 300_001).is_err());
    }

    #[test]
    fn rejects_malformed_fields_before_signature_verification() {
        let mut request = signed_request();
        request.provider_signature.push('A');
        assert!(verify_gift_receipt(SECRET, RoomId(0x1234), &request, NOW).is_err());

        request = signed_request();
        request.sender_id = "1".to_owned();
        assert!(verify_gift_receipt(SECRET, RoomId(0x1234), &request, NOW).is_err());

        request = signed_request();
        request.currency = "cny".to_owned();
        assert!(verify_gift_receipt(SECRET, RoomId(0x1234), &request, NOW).is_err());

        request = signed_request();
        request.transaction_id = "x".repeat(513);
        assert!(verify_gift_receipt(SECRET, RoomId(0x1234), &request, NOW).is_err());
    }

    #[test]
    fn rejects_receipt_totals_that_overflow_the_domain_value() {
        let mut request = signed_request();
        request.unit_value = u64::MAX;
        assert!(verify_gift_receipt(SECRET, RoomId(0x1234), &request, NOW).is_err());
    }
}
