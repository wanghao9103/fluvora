//! Buildable control, signaling, and dependency-neutral WebRTC integration example.
//!
//! Every invocation reads the short-lived token from `FLUVORA_ACCESS_TOKEN`; tokens are never
//! accepted as command-line arguments because process listings and shell history are not secret.

use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use fluvora_sdk::{CallbackWebRtcPeer, Client, RoomMode};

fn usage() {
    eprintln!(
        "usage: room_client <command> [arguments]\n\
         commands:\n\
           create <sfu|p2p|live|vod>\n\
           join <room-id>\n\
           chat <room-id> <text>\n\
           custom <room-id> <json>\n\
           ice <room-id>\n\
           sfu-offer <room-id> <offer.sdp> <answer.sdp>\n\
           p2p-signal <room-id> <recipient-id|-> <kind> <payload-json>\n\
           poll <room-id> [after]\n\
           leave <room-id>\n\
         environment:\n\
           FLUVORA_BASE_URL       default: http://127.0.0.1:8080\n\
           FLUVORA_ACCESS_TOKEN   required short-lived bearer token"
    );
}

fn argument(args: &[String], index: usize, name: &str) -> Result<String, io::Error> {
    args.get(index)
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("missing {name}")))
}

fn room_mode(value: &str) -> Result<RoomMode, io::Error> {
    match value {
        "sfu" => Ok(RoomMode::Sfu),
        "p2p" => Ok(RoomMode::P2p),
        "live" => Ok(RoomMode::Live),
        "vod" => Ok(RoomMode::Vod),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mode must be sfu, p2p, live, or vod",
        )),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "missing command").into());
    }
    let base_url =
        env::var("FLUVORA_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());
    let token = env::var("FLUVORA_ACCESS_TOKEN")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "FLUVORA_ACCESS_TOKEN is required"))?;
    let client = Client::new(base_url, token)?;
    execute_command(&client, &args).await
}

async fn execute_command(client: &Client, args: &[String]) -> Result<(), Box<dyn Error>> {
    match args[1].as_str() {
        "create" => {
            let mode = room_mode(&argument(args, 2, "mode")?)?;
            let room = client.create_room(mode, Some(64), Some(16)).await?;
            println!("{}", serde_json::to_string_pretty(&room)?);
        }
        "join" => {
            let result = client.join(&argument(args, 2, "room-id")?).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        "chat" => {
            let room_id = argument(args, 2, "room-id")?;
            let text = argument(args, 3, "text")?;
            let message_id = format!("rust-demo-{}", std::process::id());
            let result = client.send_chat(&room_id, &message_id, &text).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        "custom" => {
            let room_id = argument(args, 2, "room-id")?;
            let payload = serde_json::from_str(&argument(args, 3, "json")?)?;
            let result = client
                .send_custom_data(&room_id, "demo.rust", 1, payload)
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        "ice" => {
            let result = client
                .get_ice_configuration(&argument(args, 2, "room-id")?)
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        "sfu-offer" => {
            connect_sfu_from_file(client, args).await?;
        }
        "p2p-signal" => {
            let room_id = argument(args, 2, "room-id")?;
            let recipient = argument(args, 3, "recipient-id")?;
            let kind = argument(args, 4, "kind")?;
            let payload = serde_json::from_str(&argument(args, 5, "payload-json")?)?;
            let signal = client
                .post_signal(
                    &room_id,
                    (recipient != "-").then_some(recipient),
                    kind,
                    payload,
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&signal)?);
        }
        "poll" => {
            let room_id = argument(args, 2, "room-id")?;
            let after = args.get(3).map_or(Ok(0), |value| value.parse::<u64>())?;
            let result = client.poll_signals(&room_id, after).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        "leave" => {
            let result = client.leave(&argument(args, 2, "room-id")?).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        _ => {
            usage();
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "unknown command").into());
        }
    }
    Ok(())
}

async fn connect_sfu_from_file(client: &Client, args: &[String]) -> Result<(), Box<dyn Error>> {
    let room_id = argument(args, 2, "room-id")?;
    let offer_path = argument(args, 3, "offer.sdp")?;
    let answer_path = argument(args, 4, "answer.sdp")?;
    let offer = fs::read_to_string(offer_path)?;
    let answer_slot = Arc::new(Mutex::new(None::<String>));
    let answer_writer = Arc::clone(&answer_slot);
    let mut peer = CallbackWebRtcPeer::new(
        move || {
            let value = offer.clone();
            Box::pin(async move { Ok(value) })
        },
        move |answer| {
            let destination = Arc::clone(&answer_writer);
            Box::pin(async move {
                *destination.lock().unwrap_or_else(PoisonError::into_inner) = Some(answer);
                Ok(())
            })
        },
    )
    .with_room_data_channel(|| {
        // Create reliable/ordered `fluvora.room.v1` in the host WebRTC engine here.
        Box::pin(async { Ok(()) })
    });
    let session = client.connect_sfu(&room_id, &mut peer).await?;
    let answer = answer_slot
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .ok_or_else(|| io::Error::other("server answer callback was not invoked"))?;
    fs::write(answer_path, answer)?;
    println!("{}", serde_json::to_string_pretty(&session)?);
    Ok(())
}
