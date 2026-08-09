#![no_main]

use fluvora_stun::Message;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    if let Ok(message) = Message::parse(input) {
        let _ = message.username();
        let _ = message.software();
        let _ = message.priority();
        let _ = message.ice_controlling();
        let _ = message.ice_controlled();
        let _ = message.xor_mapped_address();
        let _ = message.verify_fingerprint();
    }
});

