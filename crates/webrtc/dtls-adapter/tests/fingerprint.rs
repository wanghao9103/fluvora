use fluvora_dtls_adapter::{
    DtlsError, DtlsRole, DtlsSrtpProfile, Sha256Fingerprint, split_srtp_exporter,
};

#[test]
fn parses_and_formats_browser_fingerprint() {
    let text = (0_u8..32)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    let parsed = Sha256Fingerprint::parse("sha-256", &text).expect("valid fingerprint");
    assert_eq!(parsed.to_string(), text);
    assert!(matches!(
        Sha256Fingerprint::parse("sha-1", &text),
        Err(DtlsError::UnsupportedFingerprintAlgorithm(algorithm)) if algorithm == "sha-1"
    ));
}

#[test]
fn validates_exporter_length_and_profile_names() {
    assert!(DtlsSrtpProfile::parse_name("SRTP_AES128_CM_SHA1_80").is_ok());
    assert!(DtlsSrtpProfile::parse_name("AEAD_AES_256_GCM").is_err());
    assert!(matches!(
        split_srtp_exporter(DtlsSrtpProfile::Aes128CmSha1_80, DtlsRole::Server, &[0; 59]),
        Err(DtlsError::InvalidExporterLength {
            expected: 60,
            actual: 59
        })
    ));
}
