//! Repeatable, dependency-free performance gate for the Fluvora media hot path.

use std::env;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use fluvora_media_codec::Codec;
use fluvora_rtp::PacketBuilder;
use fluvora_sfu_core::{
    Encoding, Layer, MediaKind, ParticipantId, PublishedTrack, Room, RoomConfig,
    SubscriptionConfig, SubscriptionId, TrackId,
};
use serde_json::json;

const PUBLISHER: ParticipantId = ParticipantId(1);
const TRACK: TrackId = TrackId(1);

#[derive(Debug, Clone, Copy)]
struct Configuration {
    packets: usize,
    subscribers: usize,
    payload_bytes: usize,
    minimum_output_packets_per_second: f64,
    maximum_p99_micros: u64,
    enforce: bool,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            packets: 100_000,
            subscribers: 64,
            payload_bytes: 1_000,
            minimum_output_packets_per_second: 100_000.0,
            maximum_p99_micros: 20_000,
            enforce: false,
        }
    }
}

#[derive(Debug)]
struct ResultSummary {
    elapsed: Duration,
    output_packets: u64,
    output_bytes: u64,
    checksum: u64,
    p50_micros: u64,
    p95_micros: u64,
    p99_micros: u64,
}

#[allow(clippy::cast_precision_loss)]
fn main() -> ExitCode {
    let configuration = match parse_arguments(env::args().skip(1)) {
        Ok(configuration) => configuration,
        Err(message) => {
            eprintln!("fluvora-perf-lab: {message}");
            return ExitCode::from(2);
        }
    };
    let result = match run_sfu(&configuration) {
        Ok(result) => result,
        Err(message) => {
            eprintln!("fluvora-perf-lab: {message}");
            return ExitCode::FAILURE;
        }
    };
    let elapsed_seconds = result.elapsed.as_secs_f64();
    let output_packets_per_second = result.output_packets as f64 / elapsed_seconds;
    let output_gigabits_per_second =
        result.output_bytes as f64 * 8.0 / elapsed_seconds / 1_000_000_000.0;
    let passed = output_packets_per_second >= configuration.minimum_output_packets_per_second
        && result.p99_micros <= configuration.maximum_p99_micros;
    println!(
        "{}",
        json!({
            "schema": "fluvora.perf.sfu.v1",
            "profile": if configuration.packets <= 10_000 { "quick" } else { "capacity" },
            "input_packets": configuration.packets,
            "subscribers": configuration.subscribers,
            "payload_bytes": configuration.payload_bytes,
            "output_packets": result.output_packets,
            "output_bytes": result.output_bytes,
            "elapsed_millis": result.elapsed.as_millis(),
            "output_packets_per_second": output_packets_per_second.round(),
            "output_gigabits_per_second": (output_gigabits_per_second * 1_000.0).round() / 1_000.0,
            "input_latency_micros": {
                "p50": result.p50_micros,
                "p95": result.p95_micros,
                "p99": result.p99_micros,
            },
            "checksum": result.checksum,
            "thresholds": {
                "minimum_output_packets_per_second":
                    configuration.minimum_output_packets_per_second,
                "maximum_p99_micros": configuration.maximum_p99_micros,
            },
            "passed": passed,
        })
    );
    if configuration.enforce && !passed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<Configuration, String> {
    let mut configuration = Configuration::default();
    let arguments = arguments.collect::<Vec<_>>();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--quick" => {
                configuration.packets = 10_000;
                configuration.subscribers = 16;
            }
            "--assert" => configuration.enforce = true,
            "--packets" => {
                index += 1;
                configuration.packets = parse_value(&arguments, index, "--packets")?;
            }
            "--subscribers" => {
                index += 1;
                configuration.subscribers = parse_value(&arguments, index, "--subscribers")?;
            }
            "--payload-bytes" => {
                index += 1;
                configuration.payload_bytes = parse_value(&arguments, index, "--payload-bytes")?;
            }
            "--minimum-output-pps" => {
                index += 1;
                configuration.minimum_output_packets_per_second =
                    parse_value(&arguments, index, "--minimum-output-pps")?;
            }
            "--maximum-p99-micros" => {
                index += 1;
                configuration.maximum_p99_micros =
                    parse_value(&arguments, index, "--maximum-p99-micros")?;
            }
            "--help" => {
                return Err("usage: fluvora-perf-lab [--quick] [--assert] \
                     [--packets N] [--subscribers N] [--payload-bytes N] \
                     [--minimum-output-pps N] [--maximum-p99-micros N]"
                    .to_owned());
            }
            argument => return Err(format!("unsupported argument {argument}")),
        }
        index += 1;
    }
    if !(1..=10_000_000).contains(&configuration.packets)
        || !(1..=1_024).contains(&configuration.subscribers)
        || !(2..=1_160).contains(&configuration.payload_bytes)
        || !configuration.minimum_output_packets_per_second.is_finite()
        || configuration.minimum_output_packets_per_second <= 0.0
        || configuration.maximum_p99_micros == 0
    {
        return Err("performance configuration is outside safe bounds".to_owned());
    }
    Ok(configuration)
}

fn parse_value<T: std::str::FromStr>(
    arguments: &[String],
    index: usize,
    option: &str,
) -> Result<T, String> {
    arguments
        .get(index)
        .ok_or_else(|| format!("{option} requires a value"))?
        .parse()
        .map_err(|_| format!("{option} has an invalid value"))
}

fn run_sfu(configuration: &Configuration) -> Result<ResultSummary, String> {
    let mut room = build_room(configuration.subscribers)?;
    let mut packet =
        PacketBuilder::new(96, 1, 90_000, 100, &vec![0_u8; configuration.payload_bytes])
            .marker(true)
            .build()
            .map_err(|error| error.to_string())?;
    packet[12] = 0x10;
    let expected_outputs = configuration
        .packets
        .checked_mul(configuration.subscribers)
        .ok_or_else(|| "output count overflow".to_owned())?;
    let mut output_packets = 0_u64;
    let mut output_bytes = 0_u64;
    let mut checksum = 0_u64;
    let mut latencies = Vec::with_capacity(configuration.packets);
    let started = Instant::now();
    for input_index in 0..configuration.packets {
        let sequence = u16::try_from(input_index & usize::from(u16::MAX)).unwrap_or(0);
        packet[2..4].copy_from_slice(&sequence.to_be_bytes());
        packet[4..8].copy_from_slice(
            &u32::try_from(input_index)
                .unwrap_or(u32::MAX)
                .wrapping_mul(3_000)
                .to_be_bytes(),
        );
        packet[13] = u8::from(input_index != 0);
        let packet_started = Instant::now();
        let output = room
            .handle_rtp(
                Duration::from_micros(u64::try_from(input_index).unwrap_or(u64::MAX)),
                PUBLISHER,
                &packet,
            )
            .map_err(|error| error.to_string())?;
        latencies.push(u64::try_from(packet_started.elapsed().as_micros()).unwrap_or(u64::MAX));
        output_packets =
            output_packets.saturating_add(u64::try_from(output.packets.len()).unwrap_or(u64::MAX));
        for forwarded in output.packets {
            output_bytes = output_bytes
                .saturating_add(u64::try_from(forwarded.packet.len()).unwrap_or(u64::MAX));
            checksum = checksum.wrapping_add(u64::from(forwarded.packet[2]));
        }
    }
    let elapsed = started.elapsed();
    if output_packets != u64::try_from(expected_outputs).unwrap_or(u64::MAX) {
        return Err(format!(
            "SFU produced {output_packets} outputs, expected {expected_outputs}"
        ));
    }
    latencies.sort_unstable();
    Ok(ResultSummary {
        elapsed,
        output_packets,
        output_bytes,
        checksum,
        p50_micros: percentile(&latencies, 50),
        p95_micros: percentile(&latencies, 95),
        p99_micros: percentile(&latencies, 99),
    })
}

fn build_room(subscribers: usize) -> Result<Room, String> {
    let mut room = Room::new(RoomConfig {
        max_subscriptions: subscribers,
        ..RoomConfig::default()
    });
    room.publish(PublishedTrack {
        id: TRACK,
        owner: PUBLISHER,
        kind: MediaKind::Video,
        codec: Codec::Vp8,
        clock_rate: 90_000,
        payload_type: 96,
        encodings: vec![Encoding {
            ssrc: 100,
            rid: Some("f".to_owned()),
            spatial_layer: 0,
            max_bitrate_bps: 2_500_000,
        }],
    })
    .map_err(|error| error.to_string())?;
    for subscriber_index in 0..subscribers {
        let id = u64::try_from(subscriber_index).unwrap_or(u64::MAX) + 1;
        room.subscribe(SubscriptionConfig {
            id: SubscriptionId(id),
            subscriber: ParticipantId(u128::from(id) + 1),
            track_id: TRACK,
            output_ssrc: u32::try_from(id).unwrap_or(u32::MAX) + 1_000,
            output_payload_type: 120,
            initial_layer: Layer {
                spatial: 0,
                temporal: 2,
            },
            initial_sequence_number: u16::try_from(id).unwrap_or(u16::MAX),
            initial_timestamp: u32::try_from(id).unwrap_or(u32::MAX) * 3_000,
            extension_rewrites: Vec::new(),
        })
        .map_err(|error| error.to_string())?;
    }
    Ok(room)
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let index = sorted.len().saturating_sub(1).saturating_mul(percentile) / 100;
    sorted.get(index).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{parse_arguments, percentile};

    #[test]
    fn quick_profile_is_bounded_and_enforced() {
        let configuration = parse_arguments(
            ["--quick", "--assert", "--maximum-p99-micros", "50000"]
                .into_iter()
                .map(str::to_owned),
        )
        .expect("configuration");
        assert_eq!(configuration.packets, 10_000);
        assert_eq!(configuration.subscribers, 16);
        assert!(configuration.enforce);
        assert_eq!(configuration.maximum_p99_micros, 50_000);
    }

    #[test]
    fn percentile_uses_sorted_observations() {
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 50), 3);
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 99), 4);
        assert_eq!(percentile(&[], 95), 0);
    }
}
