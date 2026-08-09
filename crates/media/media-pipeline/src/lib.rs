//! Live packaging, recording, VOD lifecycle, and safe worker process specifications.
//!
//! This crate intentionally produces process arguments instead of shell command strings. The
//! worker may use `FFmpeg` or `GStreamer` as an isolated codec/packaging backend without making either
//! one part of Fluvora's WebRTC or SFU core.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// One independently decodable CMAF media segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// Monotonic media sequence.
    pub sequence: u64,
    /// Segment duration.
    pub duration: Duration,
    /// Relative URI within the asset namespace.
    pub uri: String,
    /// Indicates that decoder state must be reset before this segment.
    pub discontinuity: bool,
    /// Optional UTC program date/time in RFC 3339 form.
    pub program_date_time: Option<String>,
}

impl Segment {
    fn validate(&self) -> Result<(), PipelineError> {
        if self.duration.is_zero() || self.duration > Duration::from_mins(1) {
            return Err(PipelineError::InvalidSegment("duration"));
        }
        validate_relative_uri(&self.uri)?;
        if self
            .program_date_time
            .as_ref()
            .is_some_and(|value| value.len() > 64 || !value.ends_with('Z'))
        {
            return Err(PipelineError::InvalidSegment("program date time"));
        }
        Ok(())
    }
}

/// Bounded HLS/CMAF live window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LivePlaylist {
    init_uri: String,
    window_size: usize,
    segments: VecDeque<Segment>,
    next_sequence: u64,
    ended: bool,
}

impl LivePlaylist {
    /// Creates a live playlist with a bounded segment window.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError`] if the initialization URI or window size is invalid.
    pub fn new(
        init_uri: impl Into<String>,
        window_size: usize,
        first_sequence: u64,
    ) -> Result<Self, PipelineError> {
        let init_uri = init_uri.into();
        validate_relative_uri(&init_uri)?;
        if !(3..=10_000).contains(&window_size) {
            return Err(PipelineError::InvalidWindow);
        }
        Ok(Self {
            init_uri,
            window_size,
            segments: VecDeque::with_capacity(window_size),
            next_sequence: first_sequence,
            ended: false,
        })
    }

    /// Revalidates a deserialized playlist before it is trusted by a service.
    ///
    /// # Errors
    ///
    /// Rejects invalid URIs, window bounds, segment fields, or a non-contiguous sequence.
    pub fn validate(&self) -> Result<(), PipelineError> {
        validate_relative_uri(&self.init_uri)?;
        if !(3..=10_000).contains(&self.window_size) || self.segments.len() > self.window_size {
            return Err(PipelineError::InvalidWindow);
        }
        let mut expected = self
            .segments
            .front()
            .map_or(self.next_sequence, |segment| segment.sequence);
        for segment in &self.segments {
            segment.validate()?;
            if segment.sequence != expected {
                return Err(PipelineError::UnexpectedSequence {
                    expected,
                    actual: segment.sequence,
                });
            }
            expected = expected
                .checked_add(1)
                .ok_or(PipelineError::SequenceExhausted)?;
        }
        if expected != self.next_sequence {
            return Err(PipelineError::UnexpectedSequence {
                expected,
                actual: self.next_sequence,
            });
        }
        Ok(())
    }

    /// Appends one segment and evicts the oldest segment beyond the live window.
    ///
    /// # Errors
    ///
    /// Rejects malformed, duplicate, skipped, or post-finalization segments.
    pub fn push(&mut self, segment: Segment) -> Result<Option<Segment>, PipelineError> {
        if self.ended {
            return Err(PipelineError::PlaylistEnded);
        }
        segment.validate()?;
        if segment.sequence != self.next_sequence {
            return Err(PipelineError::UnexpectedSequence {
                expected: self.next_sequence,
                actual: segment.sequence,
            });
        }
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(PipelineError::SequenceExhausted)?;
        self.segments.push_back(segment);
        Ok((self.segments.len() > self.window_size)
            .then(|| self.segments.pop_front())
            .flatten())
    }

    /// Marks this playlist as finite VOD/event output.
    pub fn finish(&mut self) {
        self.ended = true;
    }

    /// Returns the next expected segment sequence.
    #[must_use]
    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Produces a standards-oriented HLS media playlist.
    #[must_use]
    pub fn render(&self) -> String {
        let max_duration = self
            .segments
            .iter()
            .map(|segment| segment.duration)
            .max()
            .unwrap_or(Duration::from_secs(1));
        let target_duration =
            max_duration.as_secs().max(1) + u64::from(max_duration.subsec_nanos() > 0);
        let media_sequence = self
            .segments
            .front()
            .map_or(self.next_sequence, |segment| segment.sequence);
        let mut result = format!(
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target_duration}\n\
             #EXT-X-MEDIA-SEQUENCE:{media_sequence}\n#EXT-X-MAP:URI=\"{}\"\n",
            self.init_uri
        );
        for segment in &self.segments {
            if segment.discontinuity {
                result.push_str("#EXT-X-DISCONTINUITY\n");
            }
            if let Some(timestamp) = &segment.program_date_time {
                result.push_str("#EXT-X-PROGRAM-DATE-TIME:");
                result.push_str(timestamp);
                result.push('\n');
            }
            let seconds = segment.duration.as_secs_f64();
            let _ = write!(result, "#EXTINF:{seconds:.3},\n{}\n", segment.uri);
        }
        if self.ended {
            result.push_str("#EXT-X-ENDLIST\n");
        }
        result
    }
}

fn validate_relative_uri(uri: &str) -> Result<(), PipelineError> {
    if uri.is_empty()
        || uri.len() > 1_024
        || uri.contains('\\')
        || uri.contains('\r')
        || uri.contains('\n')
        || uri.contains('?')
        || uri.contains('#')
    {
        return Err(PipelineError::InvalidUri);
    }
    let path = Path::new(uri);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(PipelineError::InvalidUri);
    }
    Ok(())
}

/// VOD processing lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum AssetState {
    /// Metadata exists and upload may begin.
    Created,
    /// Source bytes are being uploaded.
    Uploading { received_bytes: u64 },
    /// Source object is complete and immutable.
    Uploaded { source_bytes: u64 },
    /// Media probe is running.
    Probing,
    /// Renditions are being encoded and packaged.
    Transcoding { completed_outputs: u16 },
    /// Manifest and all media objects are published.
    Ready {
        manifest_uri: String,
        duration_millis: u64,
    },
    /// Processing failed; the operation can be retried from the uploaded source.
    Failed { reason: String, retryable: bool },
    /// Asset is hidden and queued for storage deletion.
    Deleting,
    /// Asset metadata is a tombstone.
    Deleted,
}

/// Event-sourced VOD aggregate with strict lifecycle transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VodAsset {
    /// Stable asset identifier.
    pub id: String,
    /// Owning tenant.
    pub tenant_id: String,
    /// Optimistic concurrency version.
    pub version: u64,
    /// Current lifecycle.
    pub state: AssetState,
}

impl VodAsset {
    /// Creates metadata for a new VOD asset.
    ///
    /// # Errors
    ///
    /// Rejects invalid public identifiers.
    pub fn create(
        id: impl Into<String>,
        tenant_id: impl Into<String>,
    ) -> Result<Self, PipelineError> {
        let id = id.into();
        let tenant_id = tenant_id.into();
        validate_identifier(&id)?;
        validate_identifier(&tenant_id)?;
        Ok(Self {
            id,
            tenant_id,
            version: 1,
            state: AssetState::Created,
        })
    }

    /// Revalidates a deserialized VOD aggregate before it is trusted by a service.
    ///
    /// # Errors
    ///
    /// Rejects invalid identifiers, state fields, or impossible minimum lifecycle versions.
    pub fn validate(&self) -> Result<(), PipelineError> {
        validate_identifier(&self.id)?;
        validate_identifier(&self.tenant_id)?;
        let minimum_version = match &self.state {
            AssetState::Created => {
                if self.version != 1 {
                    return Err(PipelineError::InvalidAssetTransition);
                }
                1
            }
            AssetState::Uploading { .. } | AssetState::Deleting => 2,
            AssetState::Uploaded { source_bytes } => {
                if *source_bytes == 0 {
                    return Err(PipelineError::UploadSizeMismatch);
                }
                3
            }
            AssetState::Probing => 4,
            AssetState::Transcoding { .. } => 5,
            AssetState::Ready {
                manifest_uri,
                duration_millis,
            } => {
                validate_relative_uri(manifest_uri)?;
                if *duration_millis == 0 {
                    return Err(PipelineError::InvalidDuration);
                }
                6
            }
            AssetState::Failed { reason, .. } => {
                if reason.is_empty() || reason.len() > 1_024 {
                    return Err(PipelineError::InvalidFailureReason);
                }
                2
            }
            AssetState::Deleted => 3,
        };
        if self.version < minimum_version {
            return Err(PipelineError::InvalidAssetTransition);
        }
        Ok(())
    }

    /// Starts or resumes an upload.
    ///
    /// # Errors
    ///
    /// Rejects illegal lifecycle transitions or byte-count regressions.
    pub fn upload_progress(&mut self, received_bytes: u64) -> Result<(), PipelineError> {
        let previous = match self.state {
            AssetState::Created => 0,
            AssetState::Uploading { received_bytes } => received_bytes,
            _ => return Err(PipelineError::InvalidAssetTransition),
        };
        if received_bytes < previous {
            return Err(PipelineError::UploadRegression);
        }
        self.transition(AssetState::Uploading { received_bytes })
    }

    /// Completes the source upload.
    ///
    /// # Errors
    ///
    /// Rejects empty or inconsistent source sizes.
    pub fn complete_upload(&mut self, source_bytes: u64) -> Result<(), PipelineError> {
        let AssetState::Uploading { received_bytes } = self.state else {
            return Err(PipelineError::InvalidAssetTransition);
        };
        if source_bytes == 0 || source_bytes != received_bytes {
            return Err(PipelineError::UploadSizeMismatch);
        }
        self.transition(AssetState::Uploaded { source_bytes })
    }

    /// Starts media probing.
    ///
    /// # Errors
    ///
    /// Rejects a transition without a complete source.
    pub fn start_probe(&mut self) -> Result<(), PipelineError> {
        if !matches!(self.state, AssetState::Uploaded { .. }) {
            return Err(PipelineError::InvalidAssetTransition);
        }
        self.transition(AssetState::Probing)
    }

    /// Starts encoding after a successful probe.
    ///
    /// # Errors
    ///
    /// Rejects a transition outside probing or a retryable failure.
    pub fn start_transcode(&mut self) -> Result<(), PipelineError> {
        if !matches!(
            self.state,
            AssetState::Probing
                | AssetState::Failed {
                    retryable: true,
                    ..
                }
        ) {
            return Err(PipelineError::InvalidAssetTransition);
        }
        self.transition(AssetState::Transcoding {
            completed_outputs: 0,
        })
    }

    /// Advances the completed rendition count monotonically.
    ///
    /// # Errors
    ///
    /// Rejects regressions and updates outside transcoding.
    pub fn set_completed_outputs(&mut self, count: u16) -> Result<(), PipelineError> {
        let AssetState::Transcoding { completed_outputs } = self.state else {
            return Err(PipelineError::InvalidAssetTransition);
        };
        if count < completed_outputs {
            return Err(PipelineError::OutputRegression);
        }
        self.transition(AssetState::Transcoding {
            completed_outputs: count,
        })
    }

    /// Publishes the immutable manifest.
    ///
    /// # Errors
    ///
    /// Rejects malformed URIs, zero duration, or transitions outside transcoding.
    pub fn mark_ready(
        &mut self,
        manifest_uri: impl Into<String>,
        duration_millis: u64,
    ) -> Result<(), PipelineError> {
        if !matches!(self.state, AssetState::Transcoding { .. }) {
            return Err(PipelineError::InvalidAssetTransition);
        }
        let manifest_uri = manifest_uri.into();
        validate_relative_uri(&manifest_uri)?;
        if duration_millis == 0 {
            return Err(PipelineError::InvalidDuration);
        }
        self.transition(AssetState::Ready {
            manifest_uri,
            duration_millis,
        })
    }

    /// Records a bounded failure reason.
    ///
    /// # Errors
    ///
    /// Rejects empty/oversized reasons and terminal states.
    pub fn fail(
        &mut self,
        reason: impl Into<String>,
        retryable: bool,
    ) -> Result<(), PipelineError> {
        if matches!(self.state, AssetState::Deleting | AssetState::Deleted) {
            return Err(PipelineError::InvalidAssetTransition);
        }
        let reason = reason.into();
        if reason.is_empty() || reason.len() > 1_024 {
            return Err(PipelineError::InvalidFailureReason);
        }
        self.transition(AssetState::Failed { reason, retryable })
    }

    /// Begins asynchronous deletion from any non-deleted state.
    ///
    /// # Errors
    ///
    /// Rejects duplicate deletion.
    pub fn start_delete(&mut self) -> Result<(), PipelineError> {
        if matches!(self.state, AssetState::Deleting | AssetState::Deleted) {
            return Err(PipelineError::InvalidAssetTransition);
        }
        self.transition(AssetState::Deleting)
    }

    /// Commits the deletion tombstone.
    ///
    /// # Errors
    ///
    /// Rejects completion outside the deleting state.
    pub fn finish_delete(&mut self) -> Result<(), PipelineError> {
        if self.state != AssetState::Deleting {
            return Err(PipelineError::InvalidAssetTransition);
        }
        self.transition(AssetState::Deleted)
    }

    fn transition(&mut self, state: AssetState) -> Result<(), PipelineError> {
        self.version = self
            .version
            .checked_add(1)
            .ok_or(PipelineError::VersionExhausted)?;
        self.state = state;
        Ok(())
    }
}

fn validate_identifier(value: &str) -> Result<(), PipelineError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(PipelineError::InvalidIdentifier);
    }
    Ok(())
}

/// Supported isolated worker operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerOperation {
    /// Probe a source and write JSON metadata to stdout.
    Probe,
    /// Produce HLS/CMAF renditions.
    PackageHls {
        renditions: Vec<Rendition>,
        segment_duration_millis: u32,
        /// Whether the probed source contains an audio stream.
        has_audio: bool,
    },
}

/// One bounded ladder rendition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendition {
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
    /// Video bitrate.
    pub video_bitrate_bps: u64,
    /// Audio bitrate.
    pub audio_bitrate_bps: u32,
}

/// Process invocation passed directly to [`std::process::Command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    /// Executable path.
    pub program: PathBuf,
    /// Individual arguments; never interpreted by a command shell.
    pub arguments: Vec<OsString>,
    /// Optional isolated working directory for relative media artifacts.
    pub working_directory: Option<PathBuf>,
}

/// Bounded low-latency HLS process configuration for local RTP inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePackageSpec {
    /// Whether SDP contains at least one video stream.
    pub has_video: bool,
    /// Whether SDP contains at least one audio stream.
    pub has_audio: bool,
    /// Segment target duration.
    pub segment_duration_millis: u32,
    /// Number of media segments retained in the live manifest.
    pub window_segments: usize,
    /// Optional ABR ladder. Empty preserves the legacy single-output live manifest.
    pub renditions: Vec<Rendition>,
}

/// Codec supported by the isolated realtime transcoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeCodec {
    /// Opus audio.
    Opus,
    /// VP8 video.
    Vp8,
    /// VP9 video.
    Vp9,
    /// H.264/AVC video.
    H264,
    /// AV1 video.
    Av1,
}

impl RealtimeCodec {
    /// Returns whether this codec carries audio.
    #[must_use]
    pub const fn is_audio(self) -> bool {
        matches!(self, Self::Opus)
    }
}

/// Bounded one-input/one-output realtime transcode process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeTranscodeSpec {
    /// Encoded output codec.
    pub target_codec: RealtimeCodec,
    /// Output RTP destination; it must be a loopback socket owned by the media node.
    pub destination: SocketAddr,
    /// Dynamic RTP payload type negotiated with the subscriber.
    pub payload_type: u8,
    /// Stable output SSRC registered in the SFU.
    pub ssrc: u32,
    /// Video width; zero for audio.
    pub width: u16,
    /// Video height; zero for audio.
    pub height: u16,
    /// Video frame rate; zero for audio.
    pub frames_per_second: u16,
    /// Encoder target bitrate.
    pub bitrate_bps: u64,
}

/// Builds an isolated low-delay `FFmpeg` invocation that converts one SDP-described RTP stream and
/// returns its encoded RTP output to a trusted media-node loopback socket.
///
/// # Errors
///
/// Rejects unsafe paths, non-loopback output, incompatible media dimensions, and unbounded encoder
/// settings.
pub fn build_realtime_transcode_process(
    program: impl Into<PathBuf>,
    input_sdp: &Path,
    configuration: RealtimeTranscodeSpec,
) -> Result<ProcessSpec, PipelineError> {
    let program = program.into();
    validate_worker_path(&program, false)?;
    validate_worker_path(input_sdp, false)?;
    let audio = configuration.target_codec.is_audio();
    validate_realtime_configuration(configuration, audio)?;
    let mut arguments = vec![
        OsString::from("-nostdin"),
        OsString::from("-protocol_whitelist"),
        OsString::from("file,udp,rtp"),
        OsString::from("-fflags"),
        OsString::from("+genpts+nobuffer+discardcorrupt"),
        OsString::from("-flags"),
        OsString::from("low_delay"),
        OsString::from("-analyzeduration"),
        OsString::from("1000000"),
        OsString::from("-probesize"),
        OsString::from("1000000"),
        OsString::from("-i"),
        input_sdp.as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from("0:0"),
    ];
    if audio {
        arguments.extend([
            OsString::from("-vn"),
            OsString::from("-c:a"),
            OsString::from("libopus"),
            OsString::from("-application"),
            OsString::from("lowdelay"),
            OsString::from("-frame_duration"),
            OsString::from("20"),
            OsString::from("-b:a"),
            OsString::from(configuration.bitrate_bps.to_string()),
        ]);
    } else {
        append_realtime_video_configuration(&mut arguments, configuration);
    }
    arguments.extend([
        OsString::from("-f"),
        OsString::from("rtp"),
        OsString::from("-payload_type"),
        OsString::from(configuration.payload_type.to_string()),
        OsString::from("-ssrc"),
        OsString::from(configuration.ssrc.to_string()),
        OsString::from("-pkt_size"),
        OsString::from("1200"),
        OsString::from(format!("rtp://{}?pkt_size=1200", configuration.destination)),
    ]);
    Ok(ProcessSpec {
        program,
        arguments,
        working_directory: None,
    })
}

fn validate_realtime_configuration(
    configuration: RealtimeTranscodeSpec,
    audio: bool,
) -> Result<(), PipelineError> {
    let valid_dimensions = if audio {
        configuration.width == 0
            && configuration.height == 0
            && configuration.frames_per_second == 0
            && (16_000..=512_000).contains(&configuration.bitrate_bps)
    } else {
        (16..=7_680).contains(&configuration.width)
            && (16..=4_320).contains(&configuration.height)
            && configuration.width.is_multiple_of(2)
            && configuration.height.is_multiple_of(2)
            && (1..=120).contains(&configuration.frames_per_second)
            && (50_000..=100_000_000).contains(&configuration.bitrate_bps)
    };
    if !configuration.destination.ip().is_loopback()
        || !(96..=127).contains(&configuration.payload_type)
        || configuration.ssrc == 0
        || !valid_dimensions
    {
        return Err(PipelineError::InvalidWorkerSpec);
    }
    Ok(())
}

fn append_realtime_video_configuration(
    arguments: &mut Vec<OsString>,
    configuration: RealtimeTranscodeSpec,
) {
    arguments.extend([
        OsString::from("-an"),
        OsString::from("-vf"),
        OsString::from(format!(
            "scale=w={}:h={}:force_original_aspect_ratio=decrease,\
                 pad={}:{}:(ow-iw)/2:(oh-ih)/2",
            configuration.width, configuration.height, configuration.width, configuration.height
        )),
        OsString::from("-r"),
        OsString::from(configuration.frames_per_second.to_string()),
        OsString::from("-b:v"),
        OsString::from(configuration.bitrate_bps.to_string()),
        OsString::from("-maxrate"),
        OsString::from(
            configuration
                .bitrate_bps
                .saturating_mul(11)
                .saturating_div(10)
                .to_string(),
        ),
        OsString::from("-bufsize"),
        OsString::from(configuration.bitrate_bps.saturating_mul(2).to_string()),
        OsString::from("-g"),
        OsString::from(
            configuration
                .frames_per_second
                .saturating_mul(2)
                .to_string(),
        ),
    ]);
    append_realtime_video_encoder(arguments, configuration.target_codec);
}

fn append_realtime_video_encoder(arguments: &mut Vec<OsString>, codec: RealtimeCodec) {
    match codec {
        RealtimeCodec::H264 => arguments.extend([
            OsString::from("-c:v"),
            OsString::from("libx264"),
            OsString::from("-preset"),
            OsString::from("veryfast"),
            OsString::from("-tune"),
            OsString::from("zerolatency"),
            OsString::from("-pix_fmt"),
            OsString::from("yuv420p"),
        ]),
        RealtimeCodec::Vp8 | RealtimeCodec::Vp9 => arguments.extend([
            OsString::from("-c:v"),
            OsString::from(if codec == RealtimeCodec::Vp8 {
                "libvpx"
            } else {
                "libvpx-vp9"
            }),
            OsString::from("-deadline"),
            OsString::from("realtime"),
            OsString::from("-cpu-used"),
            OsString::from("8"),
            OsString::from("-lag-in-frames"),
            OsString::from("0"),
        ]),
        RealtimeCodec::Av1 => arguments.extend([
            OsString::from("-c:v"),
            OsString::from("libaom-av1"),
            OsString::from("-cpu-used"),
            OsString::from("8"),
            OsString::from("-row-mt"),
            OsString::from("1"),
            OsString::from("-lag-in-frames"),
            OsString::from("0"),
        ]),
        RealtimeCodec::Opus => {}
    }
}

/// Builds an isolated `FFmpeg` invocation for SDP-described loopback RTP inputs.
///
/// # Errors
///
/// Rejects unsafe paths, missing media, and unbounded segment/window parameters.
pub fn build_live_rtp_process(
    program: impl Into<PathBuf>,
    input_sdp: &Path,
    output_directory: &Path,
    configuration: &LivePackageSpec,
) -> Result<ProcessSpec, PipelineError> {
    let program = program.into();
    validate_worker_path(&program, false)?;
    validate_worker_path(input_sdp, false)?;
    validate_worker_path(output_directory, true)?;
    validate_live_package(configuration)?;
    let segment_seconds = f64::from(configuration.segment_duration_millis) / 1_000.0;
    let mut arguments = live_input_arguments(input_sdp);
    append_live_codec_arguments(&mut arguments, configuration, segment_seconds);
    append_live_hls_arguments(&mut arguments, configuration, segment_seconds);
    Ok(ProcessSpec {
        program,
        arguments,
        working_directory: Some(output_directory.to_path_buf()),
    })
}

fn validate_live_package(configuration: &LivePackageSpec) -> Result<(), PipelineError> {
    if (!configuration.has_video && !configuration.has_audio)
        || !(1_000..=10_000).contains(&configuration.segment_duration_millis)
        || !(3..=30).contains(&configuration.window_segments)
        || configuration.renditions.len() > 8
        || (!configuration.renditions.is_empty() && !configuration.has_video)
    {
        return Err(PipelineError::InvalidWorkerSpec);
    }
    for rendition in &configuration.renditions {
        rendition.validate()?;
    }
    Ok(())
}

fn live_input_arguments(input_sdp: &Path) -> Vec<OsString> {
    vec![
        OsString::from("-nostdin"),
        OsString::from("-protocol_whitelist"),
        OsString::from("file,udp,rtp"),
        OsString::from("-fflags"),
        OsString::from("+genpts+nobuffer"),
        OsString::from("-flags"),
        OsString::from("low_delay"),
        OsString::from("-i"),
        input_sdp.as_os_str().to_owned(),
    ]
}

fn append_live_codec_arguments(
    arguments: &mut Vec<OsString>,
    configuration: &LivePackageSpec,
    segment_seconds: f64,
) {
    let abr = !configuration.renditions.is_empty();
    if configuration.has_video {
        if abr {
            for (index, rendition) in configuration.renditions.iter().enumerate() {
                append_rendition_arguments(arguments, index, rendition, configuration.has_audio);
            }
        }
        arguments.extend([
            OsString::from("-c:v"),
            OsString::from("libx264"),
            OsString::from("-preset"),
            OsString::from("veryfast"),
            OsString::from("-tune"),
            OsString::from("zerolatency"),
            OsString::from("-sc_threshold"),
            OsString::from("0"),
            OsString::from("-force_key_frames"),
            OsString::from(format!("expr:gte(t,n_forced*{segment_seconds:.3})")),
        ]);
    } else {
        arguments.push(OsString::from("-vn"));
    }
    if configuration.has_audio {
        arguments.extend([OsString::from("-c:a"), OsString::from("aac")]);
        if !abr {
            arguments.extend([OsString::from("-b:a"), OsString::from("128000")]);
        }
    } else {
        arguments.push(OsString::from("-an"));
    }
}

fn append_live_hls_arguments(
    arguments: &mut Vec<OsString>,
    configuration: &LivePackageSpec,
    segment_seconds: f64,
) {
    let abr = !configuration.renditions.is_empty();
    arguments.extend([
        OsString::from("-f"),
        OsString::from("hls"),
        OsString::from("-hls_segment_type"),
        OsString::from("fmp4"),
        OsString::from("-hls_time"),
        OsString::from(format!("{segment_seconds:.3}")),
        OsString::from("-hls_list_size"),
        OsString::from(configuration.window_segments.to_string()),
        OsString::from("-hls_delete_threshold"),
        OsString::from("2"),
        OsString::from("-hls_flags"),
        OsString::from(
            "delete_segments+append_list+independent_segments+program_date_time+temp_file",
        ),
    ]);
    if abr {
        let variant_map = configuration
            .renditions
            .iter()
            .enumerate()
            .map(|(index, _)| {
                if configuration.has_audio {
                    format!("v:{index},a:{index},name:{index}")
                } else {
                    format!("v:{index},name:{index}")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        arguments.extend([
            OsString::from("-hls_fmp4_init_filename"),
            OsString::from(if configuration.renditions.len() == 1 {
                "init_0.mp4"
            } else {
                "init_%v.mp4"
            }),
            OsString::from("-hls_segment_filename"),
            OsString::from("rendition_%v_segment-%09d.m4s"),
            OsString::from("-master_pl_name"),
            OsString::from("master.m3u8"),
            OsString::from("-var_stream_map"),
            OsString::from(variant_map),
            OsString::from("rendition_%v.m3u8"),
        ]);
    } else {
        arguments.extend([
            OsString::from("-hls_fmp4_init_filename"),
            OsString::from("init.mp4"),
            OsString::from("-hls_segment_filename"),
            OsString::from("segment-%09d.m4s"),
            OsString::from("index.m3u8"),
        ]);
    }
}

/// Builds a safe, bounded FFmpeg/FFprobe invocation.
///
/// # Errors
///
/// Rejects invalid paths, traversal, oversized ladders, and unsafe encoding ranges.
pub fn build_worker_process(
    program: impl Into<PathBuf>,
    input: &Path,
    output_directory: &Path,
    operation: &WorkerOperation,
) -> Result<ProcessSpec, PipelineError> {
    let program = program.into();
    validate_worker_path(&program, false)?;
    validate_worker_path(input, false)?;
    validate_worker_path(output_directory, true)?;
    let arguments = match operation {
        WorkerOperation::Probe => vec![
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-show_format"),
            OsString::from("-show_streams"),
            OsString::from("-of"),
            OsString::from("json"),
            input.as_os_str().to_owned(),
        ],
        WorkerOperation::PackageHls {
            renditions,
            segment_duration_millis,
            has_audio,
        } => build_package_hls_arguments(input, renditions, *segment_duration_millis, *has_audio)?,
    };
    Ok(ProcessSpec {
        program,
        arguments,
        working_directory: Some(output_directory.to_path_buf()),
    })
}

fn build_package_hls_arguments(
    input: &Path,
    renditions: &[Rendition],
    segment_duration_millis: u32,
    has_audio: bool,
) -> Result<Vec<OsString>, PipelineError> {
    if renditions.is_empty()
        || renditions.len() > 8
        || !(1_000..=10_000).contains(&segment_duration_millis)
    {
        return Err(PipelineError::InvalidWorkerSpec);
    }
    for rendition in renditions {
        rendition.validate()?;
    }
    let mut arguments = vec![
        OsString::from("-nostdin"),
        OsString::from("-y"),
        OsString::from("-i"),
        input.as_os_str().to_owned(),
    ];
    for (index, rendition) in renditions.iter().enumerate() {
        append_rendition_arguments(&mut arguments, index, rendition, has_audio);
    }
    let segment_seconds = f64::from(segment_duration_millis) / 1_000.0;
    let variant_map = renditions
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if has_audio {
                format!("v:{index},a:{index},name:{index}")
            } else {
                format!("v:{index},name:{index}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    arguments.extend([
        OsString::from("-c:v"),
        OsString::from("libx264"),
        OsString::from("-preset"),
        OsString::from("veryfast"),
        OsString::from("-pix_fmt"),
        OsString::from("yuv420p"),
        OsString::from("-sc_threshold"),
        OsString::from("0"),
        OsString::from("-force_key_frames"),
        OsString::from(format!("expr:gte(t,n_forced*{segment_seconds:.3})")),
    ]);
    if has_audio {
        arguments.extend([OsString::from("-c:a"), OsString::from("aac")]);
    }
    append_hls_output_arguments(
        &mut arguments,
        segment_seconds,
        variant_map,
        renditions.len(),
    );
    Ok(arguments)
}

fn append_rendition_arguments(
    arguments: &mut Vec<OsString>,
    index: usize,
    rendition: &Rendition,
    has_audio: bool,
) {
    arguments.extend([
        OsString::from("-map"),
        OsString::from("0:v:0"),
        OsString::from(format!("-filter:v:{index}")),
        OsString::from(format!(
            "scale=w={}:h={}:force_original_aspect_ratio=decrease,\
             pad={}:{}:(ow-iw)/2:(oh-ih)/2",
            rendition.width, rendition.height, rendition.width, rendition.height
        )),
        OsString::from(format!("-b:v:{index}")),
        OsString::from(rendition.video_bitrate_bps.to_string()),
        OsString::from(format!("-maxrate:v:{index}")),
        OsString::from(
            rendition
                .video_bitrate_bps
                .saturating_mul(11)
                .saturating_div(10)
                .to_string(),
        ),
        OsString::from(format!("-bufsize:v:{index}")),
        OsString::from(rendition.video_bitrate_bps.saturating_mul(2).to_string()),
    ]);
    if has_audio {
        arguments.extend([
            OsString::from("-map"),
            OsString::from("0:a:0"),
            OsString::from(format!("-b:a:{index}")),
            OsString::from(rendition.audio_bitrate_bps.to_string()),
        ]);
    }
}

fn append_hls_output_arguments(
    arguments: &mut Vec<OsString>,
    segment_seconds: f64,
    variant_map: String,
    rendition_count: usize,
) {
    let init_filename = if rendition_count == 1 {
        OsString::from("init_0.mp4")
    } else {
        OsString::from("init_%v.mp4")
    };
    arguments.extend([
        OsString::from("-f"),
        OsString::from("hls"),
        OsString::from("-hls_segment_type"),
        OsString::from("fmp4"),
        OsString::from("-hls_time"),
        OsString::from(format!("{segment_seconds:.3}")),
        OsString::from("-hls_playlist_type"),
        OsString::from("vod"),
        OsString::from("-hls_list_size"),
        OsString::from("0"),
        OsString::from("-hls_flags"),
        OsString::from("independent_segments+temp_file"),
        OsString::from("-hls_fmp4_init_filename"),
        init_filename,
        OsString::from("-hls_segment_filename"),
        OsString::from("rendition_%v_%06d.m4s"),
        OsString::from("-master_pl_name"),
        OsString::from("master.m3u8"),
        OsString::from("-var_stream_map"),
        OsString::from(variant_map),
        OsString::from("rendition_%v.m3u8"),
    ]);
}

impl Rendition {
    fn validate(&self) -> Result<(), PipelineError> {
        if self.width < 16
            || self.height < 16
            || self.width > 7_680
            || self.height > 4_320
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
            || !(50_000..=100_000_000).contains(&self.video_bitrate_bps)
            || !(16_000..=1_000_000).contains(&self.audio_bitrate_bps)
        {
            return Err(PipelineError::InvalidRendition);
        }
        Ok(())
    }
}

fn validate_worker_path(path: &Path, allow_missing_leaf: bool) -> Result<(), PipelineError> {
    if path.as_os_str().is_empty() || path.components().any(|part| part == Component::ParentDir) {
        return Err(PipelineError::InvalidPath);
    }
    if !allow_missing_leaf && path.file_name().is_none() {
        return Err(PipelineError::InvalidPath);
    }
    Ok(())
}

/// Live/VOD pipeline error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineError {
    /// A segment field is outside its bound.
    InvalidSegment(&'static str),
    /// Media URI is absolute, traversing, or injection-prone.
    InvalidUri,
    /// Live window is too small or excessively large.
    InvalidWindow,
    /// Playlist already has `ENDLIST`.
    PlaylistEnded,
    /// Segment sequence is not contiguous.
    UnexpectedSequence { expected: u64, actual: u64 },
    /// Media sequence overflow.
    SequenceExhausted,
    /// Asset or tenant identifier is invalid.
    InvalidIdentifier,
    /// VOD transition is not legal from the current state.
    InvalidAssetTransition,
    /// Upload progress moved backwards.
    UploadRegression,
    /// Completed upload size does not match received bytes.
    UploadSizeMismatch,
    /// Completed output count moved backwards.
    OutputRegression,
    /// Published duration is zero.
    InvalidDuration,
    /// Failure reason is empty or too large.
    InvalidFailureReason,
    /// Asset version overflow.
    VersionExhausted,
    /// Worker executable or media path is invalid.
    InvalidPath,
    /// Worker operation is invalid.
    InvalidWorkerSpec,
    /// Rendition values are outside safe bounds.
    InvalidRendition,
}

impl fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for PipelineError {}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::{
        AssetState, LivePackageSpec, LivePlaylist, PipelineError, RealtimeCodec,
        RealtimeTranscodeSpec, Rendition, Segment, VodAsset, WorkerOperation,
        build_live_rtp_process, build_realtime_transcode_process, build_worker_process,
    };

    fn segment(sequence: u64) -> Segment {
        Segment {
            sequence,
            duration: Duration::from_millis(2_001),
            uri: format!("segments/{sequence}.m4s"),
            discontinuity: false,
            program_date_time: None,
        }
    }

    #[test]
    fn live_playlist_is_bounded_contiguous_and_finalizable() {
        let mut playlist = LivePlaylist::new("init.mp4", 3, 10).expect("playlist");
        for sequence in 10..14 {
            playlist.push(segment(sequence)).expect("segment");
        }
        playlist.finish();
        let rendered = playlist.render();
        assert!(rendered.contains("#EXT-X-TARGETDURATION:3"));
        assert!(rendered.contains("#EXT-X-MEDIA-SEQUENCE:11"));
        assert!(!rendered.contains("segments/10.m4s"));
        assert!(rendered.ends_with("#EXT-X-ENDLIST\n"));
        assert_eq!(
            playlist.push(segment(14)),
            Err(PipelineError::PlaylistEnded)
        );
    }

    #[test]
    fn rejects_uri_traversal_and_sequence_gaps() {
        assert!(LivePlaylist::new("../init.mp4", 6, 0).is_err());
        let mut playlist = LivePlaylist::new("init.mp4", 6, 5).expect("playlist");
        assert!(matches!(
            playlist.push(segment(6)),
            Err(PipelineError::UnexpectedSequence {
                expected: 5,
                actual: 6
            })
        ));
    }

    #[test]
    fn vod_asset_enforces_monotonic_lifecycle() {
        let mut asset = VodAsset::create("asset_1", "tenant-a").expect("asset");
        asset.upload_progress(1_024).expect("progress");
        assert_eq!(
            asset.complete_upload(2_048),
            Err(PipelineError::UploadSizeMismatch)
        );
        asset.complete_upload(1_024).expect("upload");
        asset.start_probe().expect("probe");
        asset.start_transcode().expect("transcode");
        asset.set_completed_outputs(2).expect("outputs");
        asset
            .mark_ready("vod/asset_1/master.m3u8", 60_000)
            .expect("ready");
        assert!(matches!(asset.state, AssetState::Ready { .. }));
        asset.start_delete().expect("deleting");
        asset.finish_delete().expect("deleted");
        assert_eq!(asset.state, AssetState::Deleted);
    }

    #[test]
    fn rejects_deserialized_aggregate_states_that_bypass_constructors() {
        let mut playlist = LivePlaylist::new("init.mp4", 3, 7).expect("playlist");
        playlist.segments.push_back(segment(8));
        assert!(matches!(
            playlist.validate(),
            Err(PipelineError::UnexpectedSequence {
                expected: 9,
                actual: 7
            })
        ));

        let invalid_asset = VodAsset {
            id: "../asset".to_owned(),
            tenant_id: "tenant".to_owned(),
            version: 1,
            state: AssetState::Ready {
                manifest_uri: "../master.m3u8".to_owned(),
                duration_millis: 0,
            },
        };
        assert!(invalid_asset.validate().is_err());
    }

    #[test]
    fn worker_spec_uses_individual_arguments_and_bounds_ladder() {
        let operation = WorkerOperation::PackageHls {
            renditions: vec![Rendition {
                width: 1_280,
                height: 720,
                video_bitrate_bps: 2_000_000,
                audio_bitrate_bps: 128_000,
            }],
            segment_duration_millis: 2_000,
            has_audio: true,
        };
        let spec = build_worker_process(
            "ffmpeg",
            Path::new("input/source.mp4"),
            Path::new("output/asset"),
            &operation,
        )
        .expect("process");
        assert_eq!(spec.program, Path::new("ffmpeg"));
        assert!(spec.arguments.iter().any(|arg| arg == "-nostdin"));
        assert!(spec.arguments.iter().any(|arg| arg == "-var_stream_map"));
        assert!(spec.arguments.iter().any(|arg| arg == "v:0,a:0,name:0"));
        assert!(
            spec.arguments
                .iter()
                .any(|arg| arg.to_string_lossy().ends_with("rendition_%v.m3u8"))
        );
        assert!(spec.arguments.iter().any(|arg| arg == "init_0.mp4"));
        assert_eq!(
            spec.working_directory.as_deref(),
            Some(Path::new("output/asset"))
        );
        assert!(!spec.arguments.iter().any(|arg| arg == "cmd.exe"));
        assert!(
            build_worker_process(
                "ffmpeg",
                Path::new("../escape.mp4"),
                Path::new("output"),
                &operation
            )
            .is_err()
        );
    }

    #[test]
    fn multi_rendition_hls_init_files_are_scoped_to_the_output_directory() {
        let rendition = Rendition {
            width: 640,
            height: 360,
            video_bitrate_bps: 600_000,
            audio_bitrate_bps: 64_000,
        };
        let operation = WorkerOperation::PackageHls {
            renditions: vec![rendition.clone(), rendition],
            segment_duration_millis: 2_000,
            has_audio: true,
        };
        let spec = build_worker_process(
            "ffmpeg",
            Path::new("input/source.mp4"),
            Path::new("output/asset"),
            &operation,
        )
        .expect("process");
        assert!(spec.arguments.iter().any(|arg| arg == "init_%v.mp4"));
        assert_eq!(
            spec.working_directory.as_deref(),
            Some(Path::new("output/asset"))
        );
    }

    #[test]
    fn live_hls_uses_relative_artifacts_inside_an_isolated_directory() {
        let spec = build_live_rtp_process(
            "ffmpeg",
            Path::new("runtime/source.sdp"),
            Path::new("output/live"),
            &LivePackageSpec {
                has_video: true,
                has_audio: false,
                segment_duration_millis: 1_000,
                window_segments: 3,
                renditions: Vec::new(),
            },
        )
        .expect("process");
        assert_eq!(
            spec.working_directory.as_deref(),
            Some(Path::new("output/live"))
        );
        assert!(spec.arguments.iter().any(|arg| arg == "init.mp4"));
        assert!(spec.arguments.iter().any(|arg| arg == "segment-%09d.m4s"));
        assert!(spec.arguments.iter().any(|arg| arg == "index.m3u8"));
        assert!(!spec.arguments.iter().any(|arg| {
            let path = Path::new(arg);
            path.is_absolute() && path.extension().is_some_and(|extension| extension == "m4s")
        }));
    }

    #[test]
    fn live_abr_builds_a_bounded_relative_rendition_ladder() {
        let rendition = Rendition {
            width: 640,
            height: 360,
            video_bitrate_bps: 600_000,
            audio_bitrate_bps: 64_000,
        };
        let spec = build_live_rtp_process(
            "ffmpeg",
            Path::new("runtime/source.sdp"),
            Path::new("output/live"),
            &LivePackageSpec {
                has_video: true,
                has_audio: true,
                segment_duration_millis: 1_000,
                window_segments: 3,
                renditions: vec![
                    rendition.clone(),
                    Rendition {
                        width: 320,
                        height: 180,
                        video_bitrate_bps: 300_000,
                        audio_bitrate_bps: 32_000,
                    },
                ],
            },
        )
        .expect("ABR process");
        assert!(spec.arguments.iter().any(|arg| arg == "master.m3u8"));
        assert!(spec.arguments.iter().any(|arg| arg == "init_%v.mp4"));
        assert!(
            spec.arguments
                .iter()
                .any(|arg| arg == "rendition_%v_segment-%09d.m4s")
        );
        assert!(
            spec.arguments
                .iter()
                .any(|arg| arg == "v:0,a:0,name:0 v:1,a:1,name:1")
        );
        assert!(
            build_live_rtp_process(
                "ffmpeg",
                Path::new("runtime/source.sdp"),
                Path::new("output/live"),
                &LivePackageSpec {
                    has_video: false,
                    has_audio: true,
                    segment_duration_millis: 1_000,
                    window_segments: 3,
                    renditions: vec![rendition],
                },
            )
            .is_err()
        );
    }

    #[test]
    fn realtime_transcode_is_loopback_only_and_codec_specific() {
        let process = build_realtime_transcode_process(
            "ffmpeg",
            Path::new("runtime/source.sdp"),
            RealtimeTranscodeSpec {
                target_codec: RealtimeCodec::H264,
                destination: "127.0.0.1:41000".parse().expect("destination"),
                payload_type: 102,
                ssrc: 91,
                width: 640,
                height: 360,
                frames_per_second: 24,
                bitrate_bps: 600_000,
            },
        )
        .expect("realtime process");
        assert!(process.working_directory.is_none());
        assert!(
            process
                .arguments
                .iter()
                .any(|argument| argument == "libx264")
        );
        assert!(
            process
                .arguments
                .iter()
                .any(|argument| argument == "rtp://127.0.0.1:41000?pkt_size=1200")
        );
        assert!(
            build_realtime_transcode_process(
                "ffmpeg",
                Path::new("runtime/source.sdp"),
                RealtimeTranscodeSpec {
                    target_codec: RealtimeCodec::Vp8,
                    destination: "192.0.2.1:41000".parse().expect("destination"),
                    payload_type: 102,
                    ssrc: 91,
                    width: 640,
                    height: 360,
                    frames_per_second: 24,
                    bitrate_bps: 600_000,
                },
            )
            .is_err()
        );
    }
}
