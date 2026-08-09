use std::net::SocketAddr;

use fluvora_transcode_bridge::{JobId as TranscodeJobId, MediaCodec};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct TrackEncodingRequest {
    pub(crate) ssrc: u32,
    pub(crate) rid: Option<String>,
    pub(crate) spatial_layer: u8,
    pub(crate) max_bitrate_bps: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PublishTrackRequest {
    pub(crate) track_id: u64,
    pub(crate) kind: String,
    pub(crate) codec: String,
    pub(crate) clock_rate: u32,
    pub(crate) payload_type: u8,
    pub(crate) encodings: Vec<TrackEncodingRequest>,
    #[serde(default)]
    pub(crate) width: u16,
    #[serde(default)]
    pub(crate) height: u16,
    #[serde(default)]
    pub(crate) frames_per_second: u16,
}

#[derive(Debug, Serialize)]
pub(crate) struct MediaPublishTrack {
    pub(crate) room_id: String,
    pub(crate) participant_id: String,
    pub(crate) track_id: u64,
    pub(crate) kind: String,
    pub(crate) codec: String,
    pub(crate) clock_rate: u32,
    pub(crate) payload_type: u8,
    pub(crate) encodings: Vec<TrackEncodingRequest>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SubscribeTrackRequest {
    pub(crate) subscription_id: u64,
    pub(crate) track_id: u64,
    pub(crate) output_ssrc: u32,
    pub(crate) output_payload_type: u8,
    pub(crate) spatial_layer: u8,
    pub(crate) temporal_layer: u8,
    pub(crate) initial_sequence_number: u16,
    pub(crate) initial_timestamp: u32,
    #[serde(default)]
    pub(crate) extension_rewrites: Vec<HeaderExtensionRewriteRequest>,
    pub(crate) transport_wide_extension_id: Option<u8>,
    #[serde(default)]
    pub(crate) subscriber_codecs: Vec<String>,
    #[serde(default)]
    pub(crate) allow_transcoding: bool,
    pub(crate) network_quality: Option<String>,
    pub(crate) hls_fallback_url: Option<String>,
    pub(crate) target_width: Option<u16>,
    pub(crate) target_height: Option<u16>,
    pub(crate) target_frames_per_second: Option<u16>,
    pub(crate) target_bitrate_bps: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct HeaderExtensionRewriteRequest {
    pub(crate) source_id: u8,
    pub(crate) destination_id: Option<u8>,
    pub(crate) replacement: Option<Vec<u8>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MediaSubscribeTrack {
    pub(crate) room_id: String,
    pub(crate) participant_id: String,
    pub(crate) subscription_id: u64,
    pub(crate) track_id: u64,
    pub(crate) output_ssrc: u32,
    pub(crate) output_payload_type: u8,
    pub(crate) spatial_layer: u8,
    pub(crate) temporal_layer: u8,
    pub(crate) initial_sequence_number: u16,
    pub(crate) initial_timestamp: u32,
    pub(crate) extension_rewrites: Vec<HeaderExtensionRewriteRequest>,
    pub(crate) transport_wide_extension_id: Option<u8>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SubscribeTrackResponse {
    pub(crate) path: &'static str,
    pub(crate) source_track_id: u64,
    pub(crate) selected_track_id: Option<u64>,
    pub(crate) codec: Option<&'static str>,
    pub(crate) transcode_job_id: Option<u64>,
    pub(crate) fallback_url: Option<String>,
}

#[derive(Debug)]
pub(crate) enum SelectedMediaPath {
    Realtime {
        path: &'static str,
        track_id: u64,
        codec: MediaCodec,
        transcode_job_id: Option<TranscodeJobId>,
    },
    Hls {
        url: String,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct MediaUnsubscribeTrack {
    pub(crate) room_id: String,
    pub(crate) participant_id: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct MediaUnpublishTrack {
    pub(crate) room_id: String,
    pub(crate) participant_id: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct CreateMediaTranscodeIngress {
    pub(crate) room_id: String,
    pub(crate) participant_id: String,
    pub(crate) track_id: u64,
    pub(crate) kind: String,
    pub(crate) codec: &'static str,
    pub(crate) clock_rate: u32,
    pub(crate) payload_type: u8,
    pub(crate) ssrc: u32,
    pub(crate) max_bitrate_bps: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MediaTranscodeIngressResponse {
    pub(crate) ingress_id: u64,
    pub(crate) destination: SocketAddr,
}

#[derive(Debug, Serialize)]
pub(crate) struct CreateRealtimeWorkerJob {
    pub(crate) job_key: String,
    pub(crate) placement_resource_id: String,
    pub(crate) placement_generation: u64,
    pub(crate) source: RealtimeWorkerSource,
    pub(crate) target: RealtimeWorkerTarget,
}

#[derive(Debug, Serialize)]
pub(crate) struct RealtimeWorkerSource {
    pub(crate) track_id: u64,
    pub(crate) kind: String,
    pub(crate) codec: String,
    pub(crate) payload_type: u8,
    pub(crate) clock_rate: u32,
    pub(crate) channels: Option<u8>,
    pub(crate) fmtp: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RealtimeWorkerTarget {
    pub(crate) codec: &'static str,
    pub(crate) destination: SocketAddr,
    pub(crate) payload_type: u8,
    pub(crate) ssrc: u32,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) frames_per_second: u16,
    pub(crate) bitrate_bps: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RealtimeWorkerJobResponse {
    pub(crate) job_id: u64,
    pub(crate) source_destination: SocketAddr,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkerJobStatus {
    pub(crate) state: String,
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MediaRecordingSink {
    pub(crate) room_id: String,
    pub(crate) track_id: u64,
    pub(crate) destination: SocketAddr,
    pub(crate) source_ssrc: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct LayerRequest {
    pub(crate) spatial_layer: u8,
    pub(crate) temporal_layer: u8,
}

#[derive(Debug, Serialize)]
pub(crate) struct MediaLayerRequest {
    pub(crate) room_id: String,
    pub(crate) participant_id: String,
    pub(crate) spatial_layer: u8,
    pub(crate) temporal_layer: u8,
}
