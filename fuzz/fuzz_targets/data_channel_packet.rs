#![no_main]

use std::time::Duration;

use fluvora_data_channel::{Association, AssociationConfig};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|packet: &[u8]| {
    let mut association = Association::new(AssociationConfig {
        local_port: 5_000,
        remote_port: 5_000,
        verification_tag: 0x1122_3344,
        initial_tsn: 1,
        cookie: vec![0x55; 32],
        maximum_channels: 16,
        maximum_message_bytes: 16_384,
    })
    .expect("static valid association");
    let _ = association.handle_packet(Duration::ZERO, packet);
});
