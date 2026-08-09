use std::collections::HashMap;

use aes::Aes128;
use ctr::cipher::{KeyIvInit, StreamCipher};
use fluvora_rtcp::parse_compound;
use fluvora_rtp::{Packet, parse_header_length};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

use crate::keys::SessionKeys;
use crate::replay::{ReplayWindow, estimate_index};
use crate::{KeyingMaterial, ProtectionProfile, SrtpError};

type Aes128Ctr = ctr::Ctr128BE<Aes128>;
type HmacSha1 = Hmac<Sha1>;

const MAX_SRTP_INDEX: u64 = (1_u64 << 48) - 1;
const MAX_SRTCP_INDEX: u32 = 0x7fff_ffff;

#[derive(Debug, Clone, Copy, Default)]
struct OutboundRtpState {
    highest_index: Option<u64>,
}

impl OutboundRtpState {
    fn advance(&mut self, sequence_number: u16) -> Result<u64, SrtpError> {
        let index = estimate_index(self.highest_index, sequence_number);
        if self.highest_index.is_some_and(|highest| index <= highest) {
            return Err(SrtpError::NonMonotonicSequence);
        }
        if index > MAX_SRTP_INDEX {
            return Err(SrtpError::PacketIndexExhausted);
        }
        self.highest_index = Some(index);
        Ok(index)
    }
}

/// Bidirectional SRTP/SRTCP cryptographic and replay state.
#[derive(Debug)]
pub struct SrtpContext {
    profile: ProtectionProfile,
    outbound_rtp_keys: SessionKeys,
    inbound_rtp_keys: SessionKeys,
    outbound_rtcp_keys: SessionKeys,
    inbound_rtcp_keys: SessionKeys,
    outbound_rtp: HashMap<u32, OutboundRtpState>,
    inbound_rtp: HashMap<u32, ReplayWindow>,
    outbound_rtcp_index: u32,
    inbound_rtcp: HashMap<u32, ReplayWindow>,
}

impl SrtpContext {
    /// Derives direction-specific SRTP and SRTCP session keys.
    #[must_use]
    pub fn new(
        profile: ProtectionProfile,
        outbound: &KeyingMaterial,
        inbound: &KeyingMaterial,
    ) -> Self {
        Self {
            profile,
            outbound_rtp_keys: outbound.derive_srtp(),
            inbound_rtp_keys: inbound.derive_srtp(),
            outbound_rtcp_keys: outbound.derive_srtcp(),
            inbound_rtcp_keys: inbound.derive_srtcp(),
            outbound_rtp: HashMap::new(),
            inbound_rtp: HashMap::new(),
            outbound_rtcp_index: 0,
            inbound_rtcp: HashMap::new(),
        }
    }

    /// Encrypts an RTP payload and appends the profile authentication tag.
    ///
    /// # Errors
    ///
    /// Returns [`SrtpError`] for malformed RTP or sequence reuse.
    pub fn protect_rtp(&mut self, packet: &mut Vec<u8>) -> Result<(), SrtpError> {
        let (header_len, sequence_number, ssrc) = {
            let parsed = Packet::parse(packet)?;
            (
                parsed.header_len(),
                parsed.header().sequence_number,
                parsed.header().ssrc,
            )
        };
        let index = self
            .outbound_rtp
            .entry(ssrc)
            .or_default()
            .advance(sequence_number)?;
        crypt(
            packet
                .get_mut(header_len..)
                .ok_or(SrtpError::AuthenticationFailed)?,
            &self.outbound_rtp_keys,
            ssrc,
            index,
        );
        let roc = u32::try_from(index >> 16).map_err(|_| SrtpError::PacketIndexExhausted)?;
        let tag = authenticate_rtp(&self.outbound_rtp_keys, packet, roc)?;
        packet.extend_from_slice(&tag[..self.profile.rtp_tag_len()]);
        Ok(())
    }

    /// Authenticates, replay-checks, and decrypts one SRTP packet.
    ///
    /// # Errors
    ///
    /// Returns [`SrtpError`] without releasing plaintext when authentication or replay checks fail.
    pub fn unprotect_rtp(&mut self, packet: &[u8]) -> Result<Vec<u8>, SrtpError> {
        let tag_len = self.profile.rtp_tag_len();
        let encrypted_len = packet
            .len()
            .checked_sub(tag_len)
            .ok_or(SrtpError::AuthenticationFailed)?;
        let encrypted = packet
            .get(..encrypted_len)
            .ok_or(SrtpError::AuthenticationFailed)?;
        let received_tag = packet
            .get(encrypted_len..)
            .ok_or(SrtpError::AuthenticationFailed)?;
        let header_len = parse_header_length(encrypted)?;
        let sequence_number = read_u16(encrypted, 2)?;
        let ssrc = read_u32(encrypted, 8)?;
        let replay = self.inbound_rtp.get(&ssrc).copied().unwrap_or_default();
        let index = estimate_index(replay.maximum(), sequence_number);
        replay.check(index)?;
        let roc = u32::try_from(index >> 16).map_err(|_| SrtpError::PacketIndexExhausted)?;
        let expected = authenticate_rtp(&self.inbound_rtp_keys, encrypted, roc)?;
        verify_tag(&expected[..tag_len], received_tag)?;

        let mut plaintext = encrypted.to_vec();
        crypt(
            plaintext
                .get_mut(header_len..)
                .ok_or(SrtpError::AuthenticationFailed)?,
            &self.inbound_rtp_keys,
            ssrc,
            index,
        );
        Packet::parse(&plaintext)?;
        self.inbound_rtp.entry(ssrc).or_default().accept(index);
        Ok(plaintext)
    }

    /// Encrypts an RTCP compound packet and appends its E/index word and authentication tag.
    ///
    /// # Errors
    ///
    /// Returns [`SrtpError`] for malformed RTCP or an exhausted 31-bit index.
    pub fn protect_rtcp(&mut self, packet: &mut Vec<u8>) -> Result<(), SrtpError> {
        parse_compound(packet)?;
        if packet.len() < 8 {
            return Err(SrtpError::SrtcpPacketTooShort(packet.len()));
        }
        if self.outbound_rtcp_index > MAX_SRTCP_INDEX {
            return Err(SrtpError::PacketIndexExhausted);
        }
        let index = self.outbound_rtcp_index;
        self.outbound_rtcp_index = self
            .outbound_rtcp_index
            .checked_add(1)
            .ok_or(SrtpError::PacketIndexExhausted)?;
        let ssrc = read_u32(packet, 4)?;
        let packet_len = packet.len();
        crypt(
            packet
                .get_mut(8..)
                .ok_or(SrtpError::SrtcpPacketTooShort(packet_len))?,
            &self.outbound_rtcp_keys,
            ssrc,
            u64::from(index),
        );
        packet.extend_from_slice(&(index | 0x8000_0000).to_be_bytes());
        let tag = authenticate(&self.outbound_rtcp_keys, packet)?;
        packet.extend_from_slice(&tag[..self.profile.rtcp_tag_len()]);
        Ok(())
    }

    /// Authenticates, replay-checks, and decrypts one SRTCP packet.
    ///
    /// # Errors
    ///
    /// Returns [`SrtpError`] without releasing plaintext on authentication or replay failure.
    pub fn unprotect_rtcp(&mut self, packet: &[u8]) -> Result<Vec<u8>, SrtpError> {
        let tag_len = self.profile.rtcp_tag_len();
        let authenticated_len = packet
            .len()
            .checked_sub(tag_len)
            .ok_or(SrtpError::SrtcpPacketTooShort(packet.len()))?;
        if authenticated_len < 12 {
            return Err(SrtpError::SrtcpPacketTooShort(packet.len()));
        }
        let authenticated = packet
            .get(..authenticated_len)
            .ok_or(SrtpError::SrtcpPacketTooShort(packet.len()))?;
        let received_tag = packet
            .get(authenticated_len..)
            .ok_or(SrtpError::SrtcpPacketTooShort(packet.len()))?;
        let index_offset = authenticated_len - 4;
        let index_word = read_u32(authenticated, index_offset)?;
        let encrypted = index_word & 0x8000_0000 != 0;
        let index = index_word & MAX_SRTCP_INDEX;
        let ssrc = read_u32(authenticated, 4)?;
        let replay = self.inbound_rtcp.get(&ssrc).copied().unwrap_or_default();
        replay.check(u64::from(index))?;
        let expected = authenticate(&self.inbound_rtcp_keys, authenticated)?;
        verify_tag(&expected[..tag_len], received_tag)?;

        let mut plaintext = authenticated
            .get(..index_offset)
            .ok_or(SrtpError::SrtcpPacketTooShort(packet.len()))?
            .to_vec();
        if encrypted {
            let plaintext_len = plaintext.len();
            crypt(
                plaintext
                    .get_mut(8..)
                    .ok_or(SrtpError::SrtcpPacketTooShort(plaintext_len))?,
                &self.inbound_rtcp_keys,
                ssrc,
                u64::from(index),
            );
        }
        parse_compound(&plaintext)?;
        self.inbound_rtcp
            .entry(ssrc)
            .or_default()
            .accept(u64::from(index));
        Ok(plaintext)
    }
}

fn crypt(payload: &mut [u8], keys: &SessionKeys, ssrc: u32, index: u64) {
    let mut iv = make_iv(&keys.salt, ssrc, index);
    let mut cipher = Aes128Ctr::new((&keys.encryption).into(), (&iv).into());
    cipher.apply_keystream(payload);
    iv.zeroize();
}

fn make_iv(salt: &[u8; 14], ssrc: u32, index: u64) -> [u8; 16] {
    let mut iv = [0_u8; 16];
    iv[..14].copy_from_slice(salt);
    for (output, input) in iv[4..8].iter_mut().zip(ssrc.to_be_bytes()) {
        *output ^= input;
    }
    for (output, input) in iv[8..14]
        .iter_mut()
        .zip(index.to_be_bytes()[2..].iter().copied())
    {
        *output ^= input;
    }
    iv
}

fn authenticate_rtp(
    keys: &SessionKeys,
    packet: &[u8],
    rollover_counter: u32,
) -> Result<[u8; 20], SrtpError> {
    let mut mac = HmacSha1::new_from_slice(&keys.authentication)
        .map_err(|_| SrtpError::AuthenticationFailed)?;
    mac.update(packet);
    mac.update(&rollover_counter.to_be_bytes());
    Ok(mac.finalize().into_bytes().into())
}

fn authenticate(keys: &SessionKeys, packet: &[u8]) -> Result<[u8; 20], SrtpError> {
    let mut mac = HmacSha1::new_from_slice(&keys.authentication)
        .map_err(|_| SrtpError::AuthenticationFailed)?;
    mac.update(packet);
    Ok(mac.finalize().into_bytes().into())
}

fn verify_tag(expected: &[u8], received: &[u8]) -> Result<(), SrtpError> {
    if expected.len() == received.len() && bool::from(expected.ct_eq(received)) {
        Ok(())
    } else {
        Err(SrtpError::AuthenticationFailed)
    }
}

fn read_u16(input: &[u8], position: usize) -> Result<u16, SrtpError> {
    let bytes: [u8; 2] = input
        .get(position..position.saturating_add(2))
        .ok_or(SrtpError::AuthenticationFailed)?
        .try_into()
        .map_err(|_| SrtpError::AuthenticationFailed)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_u32(input: &[u8], position: usize) -> Result<u32, SrtpError> {
    let bytes: [u8; 4] = input
        .get(position..position.saturating_add(4))
        .ok_or(SrtpError::AuthenticationFailed)?
        .try_into()
        .map_err(|_| SrtpError::AuthenticationFailed)?;
    Ok(u32::from_be_bytes(bytes))
}
