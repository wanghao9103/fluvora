use std::collections::HashSet;

use fluvora_sdp::{
    AnswerConfig, Direction, MediaKind, SdpErrorKind, SessionDescription, create_sfu_answer,
};

const CHROME_OFFER: &str = "v=0\r
o=- 4611733055514551064 2 IN IP4 127.0.0.1\r
s=-\r
t=0 0\r
a=group:BUNDLE 0 1\r
a=msid-semantic: WMS stream-id\r
m=audio 9 UDP/TLS/RTP/SAVPF 111 63 9 13 110 126\r
c=IN IP4 0.0.0.0\r
a=rtcp:9 IN IP4 0.0.0.0\r
a=ice-ufrag:remoteUfrag\r
a=ice-pwd:remotePasswordRemotePassword\r
a=ice-options:trickle\r
a=fingerprint:sha-256 AA:BB:CC:DD\r
a=setup:actpass\r
a=mid:0\r
a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level\r
a=sendrecv\r
a=msid:stream-id audio-id\r
a=rtcp-mux\r
a=rtpmap:111 opus/48000/2\r
a=rtcp-fb:111 transport-cc\r
a=fmtp:111 minptime=10;useinbandfec=1\r
a=rtpmap:63 red/48000/2\r
a=fmtp:63 111/111\r
a=rtpmap:9 G722/8000\r
a=rtpmap:13 CN/8000\r
a=rtpmap:110 telephone-event/48000\r
a=rtpmap:126 telephone-event/8000\r
m=video 9 UDP/TLS/RTP/SAVPF 96 97 98 99\r
c=IN IP4 0.0.0.0\r
a=rtcp:9 IN IP4 0.0.0.0\r
a=ice-ufrag:remoteUfrag\r
a=ice-pwd:remotePasswordRemotePassword\r
a=ice-options:trickle\r
a=fingerprint:sha-256 AA:BB:CC:DD\r
a=setup:actpass\r
a=mid:1\r
a=extmap:3 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time\r
a=extmap:4 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r
a=sendrecv\r
a=msid:stream-id video-id\r
a=rtcp-mux\r
a=rtcp-rsize\r
a=rtpmap:96 VP8/90000\r
a=rtcp-fb:96 goog-remb\r
a=rtcp-fb:96 transport-cc\r
a=rtcp-fb:96 nack\r
a=rtcp-fb:96 nack pli\r
a=rtpmap:97 rtx/90000\r
a=fmtp:97 apt=96\r
a=rtpmap:98 VP9/90000\r
a=rtpmap:99 rtx/90000\r
a=fmtp:99 apt=98\r
a=rid:q send max-width=320;max-height=180\r
a=rid:h send max-width=640;max-height=360\r
a=rid:f send max-width=1280;max-height=720\r
a=simulcast:send q;h;f\r
";

#[test]
fn parses_and_validates_browser_offer() {
    let offer = SessionDescription::parse(CHROME_OFFER).expect("browser offer parses");
    offer
        .validate_webrtc_offer()
        .expect("browser offer is valid");

    assert_eq!(offer.bundle_mids(), vec!["0", "1"]);
    assert_eq!(offer.media.len(), 2);
    assert_eq!(offer.media[0].kind, MediaKind::Audio);
    assert_eq!(offer.media[0].direction(), Direction::SendRecv);
    assert_eq!(
        offer.media[0].codecs().expect("audio codecs")[0].name,
        "opus"
    );
    assert_eq!(offer.media[1].rids().expect("video RIDs").len(), 3);
}

#[test]
fn creates_controlled_sfu_answer() {
    let offer = SessionDescription::parse(CHROME_OFFER).expect("browser offer parses");
    let mut config = AnswerConfig::mvp(
        42,
        "localUfrag",
        "localPasswordLocalPassword",
        "11:22:33:44",
    );
    config.candidates = vec!["1 1 udp 2130706431 192.0.2.10 3478 typ host".to_owned()];
    config.extension_uris = HashSet::from([
        "urn:ietf:params:rtp-hdrext:ssrc-audio-level".to_owned(),
        "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01".to_owned(),
    ]);

    let answer = create_sfu_answer(&offer, &config).expect("compatible offer");

    assert!(answer.contains("a=ice-lite\r\n"));
    assert!(answer.contains("m=audio 9 UDP/TLS/RTP/SAVPF 111"));
    assert!(answer.contains("m=video 9 UDP/TLS/RTP/SAVPF 96 97"));
    assert!(!answer.contains("VP9"));
    assert!(answer.contains("a=setup:passive"));
    assert!(answer.contains("a=rtcp-mux"));
    assert!(answer.contains("a=end-of-candidates"));

    let parsed_answer = SessionDescription::parse(&answer).expect("generated answer parses");
    assert_eq!(parsed_answer.media.len(), 2);
}

#[test]
fn rejects_missing_rtcp_mux() {
    let invalid = CHROME_OFFER.replacen("a=rtcp-mux\r\n", "", 1);
    let offer = SessionDescription::parse(&invalid).expect("syntax remains valid");
    let error = offer
        .validate_webrtc_offer()
        .expect_err("rtcp-mux is required");
    assert!(matches!(error.kind(), SdpErrorKind::MissingRtcpMux(mid) if mid == "0"));
}

#[test]
fn rejects_duplicate_mid() {
    let invalid = CHROME_OFFER.replace("a=mid:1", "a=mid:0");
    let offer = SessionDescription::parse(&invalid).expect("syntax remains valid");
    let error = offer
        .validate_webrtc_offer()
        .expect_err("MID must be unique");
    assert!(matches!(error.kind(), SdpErrorKind::DuplicateMid(mid) if mid == "0"));
}
