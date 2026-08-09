#![no_main]

use fluvora_rtp::Packet;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = Packet::parse(data);
});
