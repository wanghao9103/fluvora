//! Stable blocking C ABI for desktop, game-engine, and native-language integrations.
//!
//! Pointers received from callers are necessarily unsafe to dereference. Every exported function
//! validates nulls, catches Rust panics, and never lets an unwind cross the ABI boundary.

#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Mutex;

use fluvora_sdk::{Client, RoomMode};

const MAX_BASE_URL_BYTES: usize = 2_048;
const MAX_ACCESS_TOKEN_BYTES: usize = 4_096;
const MAX_IDENTIFIER_BYTES: usize = 32;
const MAX_NAME_BYTES: usize = 128;
const MAX_STRUCTURED_INPUT_BYTES: usize = 1_024 * 1_024;

/// Successful operation.
pub const FLUVORA_OK: i32 = 0;
/// A pointer or UTF-8 argument is invalid.
pub const FLUVORA_INVALID_ARGUMENT: i32 = 1;
/// SDK configuration or API operation failed.
pub const FLUVORA_SDK_ERROR: i32 = 2;
/// Output JSON could not be encoded as a C string.
pub const FLUVORA_ENCODING_ERROR: i32 = 3;
/// A panic was contained at the ABI boundary.
pub const FLUVORA_PANIC: i32 = 4;

/// Opaque native client handle.
#[derive(Debug)]
pub struct FluvoraClient {
    client: Client,
    runtime: Mutex<tokio::runtime::Runtime>,
}

/// Creates a blocking native client.
///
/// `base_url` and `access_token` must point to valid NUL-terminated UTF-8 for the duration of this
/// call. The returned handle must be released with [`fluvora_client_free`].
///
/// # Safety
///
/// The caller must provide valid readable C string pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_client_new(
    base_url: *const c_char,
    access_token: *const c_char,
) -> *mut FluvoraClient {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(base_url) = read_c_string(base_url, MAX_BASE_URL_BYTES) else {
            return ptr::null_mut();
        };
        let Some(access_token) = read_c_string(access_token, MAX_ACCESS_TOKEN_BYTES) else {
            return ptr::null_mut();
        };
        let Ok(client) = Client::new(base_url, access_token) else {
            return ptr::null_mut();
        };
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return ptr::null_mut();
        };
        Box::into_raw(Box::new(FluvoraClient {
            client,
            runtime: Mutex::new(runtime),
        }))
    }))
    .unwrap_or(ptr::null_mut())
}

/// Releases a native client. Passing null is allowed.
///
/// # Safety
///
/// A non-null pointer must have been returned by [`fluvora_client_new`] and not already freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_client_free(client: *mut FluvoraClient) {
    if !client.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: upheld by this function's public contract and checked for null above.
            drop(unsafe { Box::from_raw(client) });
        }));
    }
}

/// Frees a string produced through an `out_json` parameter. Passing null is allowed.
///
/// # Safety
///
/// A non-null pointer must have been returned by this library and not already freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_string_free(value: *mut c_char) {
    if !value.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: upheld by this function's public contract and checked for null above.
            drop(unsafe { CString::from_raw(value) });
        }));
    }
}

/// Replaces the bearer token after application-controlled refresh.
///
/// # Safety
///
/// The handle must be live and `access_token` must be a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_client_set_access_token(
    client: *mut FluvoraClient,
    access_token: *const c_char,
) -> i32 {
    ffi_status(|| {
        let client = client_ref(client)?;
        let token =
            read_c_string(access_token, MAX_ACCESS_TOKEN_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        client
            .client
            .set_access_token(token)
            .map_err(|_| FLUVORA_SDK_ERROR)
    })
}

/// Creates a room and writes an allocated JSON response to `out_json`.
///
/// `mode` accepts `sfu`, `p2p`, `live`, or `vod`. A zero limit uses the server default.
///
/// # Safety
///
/// All pointers must meet the contracts described in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_create_room(
    client: *mut FluvoraClient,
    mode: *const c_char,
    max_members: usize,
    max_publishers: usize,
    out_json: *mut *mut c_char,
) -> i32 {
    ffi_status(|| {
        initialize_output(out_json)?;
        let client = client_ref(client)?;
        let mode = match read_c_string(mode, MAX_NAME_BYTES).as_deref() {
            Some("sfu") => RoomMode::Sfu,
            Some("p2p") => RoomMode::P2p,
            Some("live") => RoomMode::Live,
            Some("vod") => RoomMode::Vod,
            _ => return Err(FLUVORA_INVALID_ARGUMENT),
        };
        let runtime = client.runtime.lock().map_err(|_| FLUVORA_SDK_ERROR)?;
        let room = runtime
            .block_on(client.client.create_room(
                mode,
                (max_members != 0).then_some(max_members),
                (max_publishers != 0).then_some(max_publishers),
            ))
            .map_err(|_| FLUVORA_SDK_ERROR)?;
        write_json(out_json, &room)
    })
}

/// Reads a room snapshot and writes it as JSON.
///
/// # Safety
///
/// All pointers must meet the contracts described in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_get_room(
    client: *mut FluvoraClient,
    room_id: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    ffi_status(|| {
        initialize_output(out_json)?;
        let client = client_ref(client)?;
        let room_id =
            read_c_string(room_id, MAX_IDENTIFIER_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let runtime = client.runtime.lock().map_err(|_| FLUVORA_SDK_ERROR)?;
        let room = runtime
            .block_on(client.client.get_room(&room_id))
            .map_err(|_| FLUVORA_SDK_ERROR)?;
        write_json(out_json, &room)
    })
}

/// Runs a common idempotent room command and writes its result as JSON.
///
/// `operation` accepts `join`, `leave`, `end`, `publish_start`, or `publish_stop`.
///
/// # Safety
///
/// All pointers must meet the contracts described in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_room_command(
    client: *mut FluvoraClient,
    room_id: *const c_char,
    operation: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    ffi_status(|| {
        initialize_output(out_json)?;
        let client = client_ref(client)?;
        let room_id =
            read_c_string(room_id, MAX_IDENTIFIER_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let operation = read_c_string(operation, MAX_NAME_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let runtime = client.runtime.lock().map_err(|_| FLUVORA_SDK_ERROR)?;
        let result = match operation.as_str() {
            "join" => runtime.block_on(client.client.join(&room_id)),
            "leave" => runtime.block_on(client.client.leave(&room_id)),
            "end" => runtime.block_on(client.client.end(&room_id)),
            "publish_start" => runtime.block_on(client.client.start_publishing(&room_id)),
            "publish_stop" => runtime.block_on(client.client.stop_publishing(&room_id)),
            _ => return Err(FLUVORA_INVALID_ARGUMENT),
        }
        .map_err(|_| FLUVORA_SDK_ERROR)?;
        write_json(out_json, &result)
    })
}

/// Joins a room and writes the command result JSON.
///
/// # Safety
///
/// All pointers must meet the contracts described in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_join_room(
    client: *mut FluvoraClient,
    room_id: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    ffi_status(|| {
        initialize_output(out_json)?;
        let client = client_ref(client)?;
        let room_id =
            read_c_string(room_id, MAX_IDENTIFIER_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let runtime = client.runtime.lock().map_err(|_| FLUVORA_SDK_ERROR)?;
        let result = runtime
            .block_on(client.client.join(&room_id))
            .map_err(|_| FLUVORA_SDK_ERROR)?;
        write_json(out_json, &result)
    })
}

/// Leaves a room, releases participant media, and writes the command result JSON.
///
/// # Safety
///
/// All pointers must meet the contracts described in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_leave_room(
    client: *mut FluvoraClient,
    room_id: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    ffi_status(|| {
        initialize_output(out_json)?;
        let client = client_ref(client)?;
        let room_id =
            read_c_string(room_id, MAX_IDENTIFIER_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let runtime = client.runtime.lock().map_err(|_| FLUVORA_SDK_ERROR)?;
        let result = runtime
            .block_on(client.client.leave(&room_id))
            .map_err(|_| FLUVORA_SDK_ERROR)?;
        write_json(out_json, &result)
    })
}

/// Sends durable room chat and writes the command result JSON.
///
/// # Safety
///
/// All pointers must meet the contracts described in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_send_chat(
    client: *mut FluvoraClient,
    room_id: *const c_char,
    message_id: *const c_char,
    text: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    ffi_status(|| {
        initialize_output(out_json)?;
        let client = client_ref(client)?;
        let room_id =
            read_c_string(room_id, MAX_IDENTIFIER_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let message_id =
            read_c_string(message_id, MAX_IDENTIFIER_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let text =
            read_c_string(text, MAX_STRUCTURED_INPUT_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let runtime = client.runtime.lock().map_err(|_| FLUVORA_SDK_ERROR)?;
        let result = runtime
            .block_on(client.client.send_chat(&room_id, &message_id, &text))
            .map_err(|_| FLUVORA_SDK_ERROR)?;
        write_json(out_json, &result)
    })
}

/// Sends versioned durable application JSON and writes the command result JSON.
///
/// `payload_json` must contain exactly one valid JSON value.
///
/// # Safety
///
/// All pointers must meet the contracts described in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_send_custom_data(
    client: *mut FluvoraClient,
    room_id: *const c_char,
    namespace_name: *const c_char,
    schema_version: u16,
    payload_json: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    ffi_status(|| {
        initialize_output(out_json)?;
        let client = client_ref(client)?;
        let room_id =
            read_c_string(room_id, MAX_IDENTIFIER_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let namespace_name =
            read_c_string(namespace_name, MAX_NAME_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let payload_json = read_c_string(payload_json, MAX_STRUCTURED_INPUT_BYTES)
            .ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let payload = serde_json::from_str(&payload_json).map_err(|_| FLUVORA_INVALID_ARGUMENT)?;
        let runtime = client.runtime.lock().map_err(|_| FLUVORA_SDK_ERROR)?;
        let result = runtime
            .block_on(client.client.send_custom_data(
                &room_id,
                &namespace_name,
                schema_version,
                payload,
            ))
            .map_err(|_| FLUVORA_SDK_ERROR)?;
        write_json(out_json, &result)
    })
}

/// Exchanges a native WebRTC offer and writes `{session_id, answer_sdp}` JSON.
///
/// # Safety
///
/// All pointers must meet the contracts described in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_exchange_offer(
    client: *mut FluvoraClient,
    room_id: *const c_char,
    offer_sdp: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    ffi_status(|| {
        initialize_output(out_json)?;
        let client = client_ref(client)?;
        let room_id =
            read_c_string(room_id, MAX_IDENTIFIER_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let offer_sdp =
            read_c_string(offer_sdp, MAX_STRUCTURED_INPUT_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let runtime = client.runtime.lock().map_err(|_| FLUVORA_SDK_ERROR)?;
        let session = runtime
            .block_on(client.client.exchange_offer(&room_id, &offer_sdp))
            .map_err(|_| FLUVORA_SDK_ERROR)?;
        write_json(out_json, &session)
    })
}

/// Posts one P2P signal and writes the accepted signal record JSON.
///
/// `recipient_id` may be null for a room broadcast. `payload_json` must be one complete JSON value.
///
/// # Safety
///
/// Non-null pointers must name valid readable C strings and `out_json` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_post_signal(
    client: *mut FluvoraClient,
    room_id: *const c_char,
    recipient_id: *const c_char,
    kind: *const c_char,
    payload_json: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    ffi_status(|| {
        initialize_output(out_json)?;
        let client = client_ref(client)?;
        let room_id =
            read_c_string(room_id, MAX_IDENTIFIER_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let recipient_id = read_optional_c_string(recipient_id, MAX_IDENTIFIER_BYTES)?;
        let kind = read_c_string(kind, MAX_NAME_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let payload_json = read_c_string(payload_json, MAX_STRUCTURED_INPUT_BYTES)
            .ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let payload = serde_json::from_str(&payload_json).map_err(|_| FLUVORA_INVALID_ARGUMENT)?;
        let runtime = client.runtime.lock().map_err(|_| FLUVORA_SDK_ERROR)?;
        let signal = runtime
            .block_on(
                client
                    .client
                    .post_signal(&room_id, recipient_id, kind, payload),
            )
            .map_err(|_| FLUVORA_SDK_ERROR)?;
        write_json(out_json, &signal)
    })
}

/// Polls P2P signals after an exclusive sequence and writes a JSON page.
///
/// The page shape is `{"signals":[...],"latest_sequence":n}`.
///
/// # Safety
///
/// All pointers must meet the contracts described in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_poll_signals(
    client: *mut FluvoraClient,
    room_id: *const c_char,
    after_sequence: u64,
    out_json: *mut *mut c_char,
) -> i32 {
    ffi_status(|| {
        initialize_output(out_json)?;
        let client = client_ref(client)?;
        let room_id =
            read_c_string(room_id, MAX_IDENTIFIER_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let runtime = client.runtime.lock().map_err(|_| FLUVORA_SDK_ERROR)?;
        let (signals, latest_sequence) = runtime
            .block_on(client.client.poll_signals(&room_id, after_sequence))
            .map_err(|_| FLUVORA_SDK_ERROR)?;
        write_json(
            out_json,
            &serde_json::json!({
                "signals": signals,
                "latest_sequence": latest_sequence
            }),
        )
    })
}

/// Retrieves room-scoped STUN/TURN servers and writes the configuration JSON.
///
/// # Safety
///
/// All pointers must meet the contracts described in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fluvora_get_ice_configuration(
    client: *mut FluvoraClient,
    room_id: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    ffi_status(|| {
        initialize_output(out_json)?;
        let client = client_ref(client)?;
        let room_id =
            read_c_string(room_id, MAX_IDENTIFIER_BYTES).ok_or(FLUVORA_INVALID_ARGUMENT)?;
        let runtime = client.runtime.lock().map_err(|_| FLUVORA_SDK_ERROR)?;
        let configuration = runtime
            .block_on(client.client.get_ice_configuration(&room_id))
            .map_err(|_| FLUVORA_SDK_ERROR)?;
        write_json(out_json, &configuration)
    })
}

fn ffi_status(operation: impl FnOnce() -> Result<(), i32>) -> i32 {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(())) => FLUVORA_OK,
        Ok(Err(code)) => code,
        Err(_) => FLUVORA_PANIC,
    }
}

fn client_ref<'a>(client: *mut FluvoraClient) -> Result<&'a FluvoraClient, i32> {
    if client.is_null() {
        return Err(FLUVORA_INVALID_ARGUMENT);
    }
    // SAFETY: exported functions document that handles must be live allocations from this library.
    Ok(unsafe { &*client })
}

fn read_c_string(value: *const c_char, maximum_bytes: usize) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // SAFETY: exported functions document that input pointers name readable NUL-terminated strings.
    let value = unsafe { CStr::from_ptr(value) };
    if value.to_bytes().len() > maximum_bytes {
        return None;
    }
    value.to_str().ok().map(str::to_owned)
}

fn read_optional_c_string(
    value: *const c_char,
    maximum_bytes: usize,
) -> Result<Option<String>, i32> {
    if value.is_null() {
        Ok(None)
    } else {
        read_c_string(value, maximum_bytes)
            .map(Some)
            .ok_or(FLUVORA_INVALID_ARGUMENT)
    }
}

fn initialize_output(output: *mut *mut c_char) -> Result<(), i32> {
    if output.is_null() {
        return Err(FLUVORA_INVALID_ARGUMENT);
    }
    // SAFETY: exported functions require a writable pointer-sized output location.
    unsafe {
        *output = ptr::null_mut();
    }
    Ok(())
}

fn write_json(output: *mut *mut c_char, value: &impl serde::Serialize) -> Result<(), i32> {
    let json = serde_json::to_string(value).map_err(|_| FLUVORA_ENCODING_ERROR)?;
    let value = CString::new(json).map_err(|_| FLUVORA_ENCODING_ERROR)?;
    // SAFETY: output was validated by initialize_output in every caller.
    unsafe {
        *output = value.into_raw();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        FLUVORA_INVALID_ARGUMENT, fluvora_client_free, fluvora_client_new,
        fluvora_send_custom_data, read_c_string,
    };
    use std::ffi::{CString, c_char};
    use std::ptr;

    #[test]
    fn custom_data_rejects_invalid_json_before_network_io() {
        let base_url = CString::new("http://127.0.0.1:1").expect("valid URL C string");
        let token = CString::new("short-lived-test-token").expect("valid token C string");
        let room = CString::new("a1").expect("valid room C string");
        let namespace = CString::new("test.custom").expect("valid namespace C string");
        let payload = CString::new("{").expect("valid payload C string");
        // SAFETY: every pointer is a live NUL-terminated C string for the duration of the call.
        let client = unsafe { fluvora_client_new(base_url.as_ptr(), token.as_ptr()) };
        assert!(!client.is_null());
        let mut output: *mut c_char = ptr::null_mut();
        // SAFETY: the client is live, inputs are C strings, and output is writable.
        let status = unsafe {
            fluvora_send_custom_data(
                client,
                room.as_ptr(),
                namespace.as_ptr(),
                1,
                payload.as_ptr(),
                &raw mut output,
            )
        };
        assert_eq!(status, FLUVORA_INVALID_ARGUMENT);
        assert!(output.is_null());
        // SAFETY: client came from fluvora_client_new and has not been freed.
        unsafe { fluvora_client_free(client) };
    }

    #[test]
    fn bounded_c_strings_reject_oversized_and_invalid_utf8_inputs() {
        let oversized = CString::new("x".repeat(9)).expect("valid oversized C string");
        assert!(read_c_string(oversized.as_ptr(), 8).is_none());
        let invalid_utf8 = [0xff_u8, 0];
        assert!(read_c_string(invalid_utf8.as_ptr().cast(), 8).is_none());
        assert!(read_c_string(ptr::null(), 8).is_none());
    }
}
