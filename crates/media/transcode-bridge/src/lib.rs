//! Shared transcoder allocation and media-path negotiation.

use std::collections::{HashMap, HashSet};
use std::fmt;

/// Codec family used for compatibility decisions and worker profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaCodec {
    /// Opus audio.
    Opus,
    /// AAC audio.
    Aac,
    /// VP8 video.
    Vp8,
    /// VP9 video.
    Vp9,
    /// H.264/AVC video.
    H264,
    /// AV1 video.
    Av1,
}

impl MediaCodec {
    /// Returns whether this codec carries audio rather than video.
    #[must_use]
    pub const fn is_audio(self) -> bool {
        matches!(self, Self::Opus | Self::Aac)
    }
}

/// Immutable source properties discovered from RTP negotiation or media probing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceDescriptor {
    /// Stable source/track or object identifier.
    pub source_id: String,
    /// Encoded source codec.
    pub codec: MediaCodec,
    /// Source pixel width; zero for audio.
    pub width: u16,
    /// Source pixel height; zero for audio.
    pub height: u16,
    /// Source frames per second; zero for audio.
    pub frames_per_second: u16,
}

/// One reusable transcoder output.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TranscodeSpec {
    /// Source identity and ceiling.
    pub source: SourceDescriptor,
    /// Required output codec.
    pub target_codec: MediaCodec,
    /// Output width.
    pub width: u16,
    /// Output height.
    pub height: u16,
    /// Output frames per second.
    pub frames_per_second: u16,
    /// Encoder target bitrate.
    pub bitrate_bps: u64,
}

impl TranscodeSpec {
    /// Enforces non-upscaling and bounded output properties.
    ///
    /// # Errors
    ///
    /// Returns [`TranscodeError`] for empty identities, unsupported ranges, or a requested upscale.
    pub fn validate(&self) -> Result<(), TranscodeError> {
        if self.source.source_id.is_empty() || self.source.source_id.len() > 256 {
            return Err(TranscodeError::InvalidSpec("source id"));
        }
        if self.bitrate_bps == 0 || self.bitrate_bps > 100_000_000 {
            return Err(TranscodeError::InvalidSpec("bitrate"));
        }
        let source_is_audio = self.source.width == 0 && self.source.height == 0;
        let target_is_audio = self.width == 0 && self.height == 0;
        if source_is_audio != target_is_audio
            || source_is_audio != self.source.codec.is_audio()
            || target_is_audio != self.target_codec.is_audio()
        {
            return Err(TranscodeError::InvalidSpec("media kind"));
        }
        if source_is_audio && (self.source.frames_per_second != 0 || self.frames_per_second != 0) {
            return Err(TranscodeError::InvalidSpec("audio dimensions"));
        }
        if !source_is_audio
            && (self.width == 0
                || self.height == 0
                || self.frames_per_second == 0
                || self.width > self.source.width
                || self.height > self.source.height
                || self.frames_per_second > self.source.frames_per_second)
        {
            return Err(TranscodeError::UpscaleNotAllowed);
        }
        Ok(())
    }
}

/// Stable process-local transcoder job identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobId(pub u64);

/// Worker lifecycle visible to the coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    /// Allocation exists but no worker has claimed it.
    Queued,
    /// Worker process is running.
    Running,
    /// Output track/playlist is ready for consumers.
    Ready { output_id: String },
    /// Worker stopped and may be retried.
    Failed { reason: String, attempts: u16 },
}

/// Shared allocation returned to a subscriber or packaging pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    /// Job identifier.
    pub job_id: JobId,
    /// `true` when an identical existing output was reused.
    pub reused: bool,
    /// Current lifecycle.
    pub state: JobState,
}

#[derive(Debug, Clone)]
struct Job {
    id: JobId,
    tenant_id: String,
    references: usize,
    state: JobState,
}

/// Global and per-tenant transcoder limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quotas {
    /// Maximum distinct active output specifications.
    pub global_jobs: usize,
    /// Maximum distinct jobs charged to one tenant.
    pub jobs_per_tenant: usize,
}

/// In-memory allocation state; durable job state can mirror its transitions.
#[derive(Debug)]
pub struct Coordinator {
    quotas: Quotas,
    next_job_id: u64,
    jobs: HashMap<TranscodeSpec, Job>,
}

impl Coordinator {
    /// Creates an empty bounded coordinator.
    #[must_use]
    pub fn new(quotas: Quotas) -> Self {
        Self {
            quotas,
            next_job_id: 1,
            jobs: HashMap::new(),
        }
    }

    /// Reuses an exact output or allocates one new queued job.
    ///
    /// Identical source/codec/resolution/framerate/bitrate requests share a single encoder even
    /// across subscribers.
    ///
    /// # Errors
    ///
    /// Returns [`TranscodeError`] for invalid specs or exceeded tenant/global quotas.
    pub fn acquire(
        &mut self,
        tenant_id: impl Into<String>,
        spec: TranscodeSpec,
    ) -> Result<Allocation, TranscodeError> {
        spec.validate()?;
        if let Some(job) = self.jobs.get_mut(&spec) {
            job.references = job.references.saturating_add(1);
            return Ok(Allocation {
                job_id: job.id,
                reused: true,
                state: job.state.clone(),
            });
        }
        let tenant_id = tenant_id.into();
        if tenant_id.is_empty() || tenant_id.len() > 128 {
            return Err(TranscodeError::InvalidTenant);
        }
        if self.jobs.len() >= self.quotas.global_jobs {
            return Err(TranscodeError::GlobalQuota);
        }
        let tenant_jobs = self
            .jobs
            .values()
            .filter(|job| job.tenant_id == tenant_id)
            .count();
        if tenant_jobs >= self.quotas.jobs_per_tenant {
            return Err(TranscodeError::TenantQuota);
        }
        let id = JobId(self.next_job_id);
        self.next_job_id = self
            .next_job_id
            .checked_add(1)
            .ok_or(TranscodeError::JobIdExhausted)?;
        let state = JobState::Queued;
        self.jobs.insert(
            spec,
            Job {
                id,
                tenant_id,
                references: 1,
                state: state.clone(),
            },
        );
        Ok(Allocation {
            job_id: id,
            reused: false,
            state,
        })
    }

    /// Drops one reference and removes an unused job.
    #[must_use]
    pub fn release(&mut self, job_id: JobId) -> bool {
        let key = self
            .jobs
            .iter()
            .find_map(|(spec, job)| (job.id == job_id).then(|| spec.clone()));
        let Some(key) = key else {
            return false;
        };
        let remove = if let Some(job) = self.jobs.get_mut(&key) {
            job.references = job.references.saturating_sub(1);
            job.references == 0
        } else {
            false
        };
        if remove {
            self.jobs.remove(&key);
        }
        true
    }

    /// Applies a worker lifecycle update.
    ///
    /// # Errors
    ///
    /// Returns [`TranscodeError::UnknownJob`] for a stale worker update.
    pub fn update_state(&mut self, job_id: JobId, state: JobState) -> Result<(), TranscodeError> {
        let job = self
            .jobs
            .values_mut()
            .find(|job| job.id == job_id)
            .ok_or(TranscodeError::UnknownJob(job_id))?;
        job.state = state;
        Ok(())
    }

    /// Returns active distinct encoder jobs.
    #[must_use]
    pub fn active_jobs(&self) -> usize {
        self.jobs.len()
    }

    /// Returns the immutable specification associated with a job.
    #[must_use]
    pub fn specification(&self, job_id: JobId) -> Option<&TranscodeSpec> {
        self.jobs
            .iter()
            .find_map(|(specification, job)| (job.id == job_id).then_some(specification))
    }

    /// Returns the number of active consumers sharing one encoder.
    #[must_use]
    pub fn references(&self, job_id: JobId) -> Option<usize> {
        self.jobs
            .values()
            .find_map(|job| (job.id == job_id).then_some(job.references))
    }
}

/// Subscriber network health used for path conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkQuality {
    /// Stable low-delay WebRTC path.
    Good,
    /// Constrained path that should remain adaptive.
    Constrained,
    /// Persistently unusable realtime path.
    Critical,
}

/// Policy inputs for one media-path decision.
#[derive(Debug, Clone)]
pub struct NegotiationRequest {
    /// Published source.
    pub source: SourceDescriptor,
    /// Browser/SDK decoders, in preference order.
    pub subscriber_codecs: Vec<MediaCodec>,
    /// Current path health.
    pub network_quality: NetworkQuality,
    /// Whether shared server-side transcoding is allowed.
    pub allow_transcoding: bool,
    /// Optional already-available HLS/VOD fallback URL.
    pub hls_fallback_url: Option<String>,
    /// Desired output ceiling.
    pub target_width: u16,
    /// Desired output ceiling.
    pub target_height: u16,
    /// Desired output frame rate.
    pub target_frames_per_second: u16,
    /// Desired output bitrate.
    pub target_bitrate_bps: u64,
}

/// Explicit result returned to signaling and SDK fallback logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NegotiationDecision {
    /// No decode/re-encode is needed.
    DirectForward { codec: MediaCodec },
    /// Allocate or reuse this worker output.
    Transcode(TranscodeSpec),
    /// Establish buffered HTTP playback before closing WebRTC.
    HlsFallback { url: String },
    /// No compatible bounded path exists.
    RejectCodecIncompatible,
}

/// Chooses direct forwarding, shared transcoding, or HLS fallback.
#[must_use]
pub fn negotiate(request: &NegotiationRequest) -> NegotiationDecision {
    if request.network_quality == NetworkQuality::Critical
        && let Some(url) = &request.hls_fallback_url
    {
        return NegotiationDecision::HlsFallback { url: url.clone() };
    }
    let supported: HashSet<_> = request.subscriber_codecs.iter().copied().collect();
    if supported.contains(&request.source.codec) {
        return NegotiationDecision::DirectForward {
            codec: request.source.codec,
        };
    }
    if request.allow_transcoding
        && let Some(target_codec) = request.subscriber_codecs.first().copied()
    {
        let spec = TranscodeSpec {
            source: request.source.clone(),
            target_codec,
            width: request.target_width.min(request.source.width),
            height: request.target_height.min(request.source.height),
            frames_per_second: request
                .target_frames_per_second
                .min(request.source.frames_per_second),
            bitrate_bps: request.target_bitrate_bps,
        };
        if spec.validate().is_ok() {
            return NegotiationDecision::Transcode(spec);
        }
    }
    if let Some(url) = &request.hls_fallback_url {
        NegotiationDecision::HlsFallback { url: url.clone() }
    } else {
        NegotiationDecision::RejectCodecIncompatible
    }
}

/// Allocation or worker coordination failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscodeError {
    /// Invalid spec field.
    InvalidSpec(&'static str),
    /// Requested output would upscale or increase frame rate.
    UpscaleNotAllowed,
    /// Tenant identifier is invalid.
    InvalidTenant,
    /// Global distinct-job limit reached.
    GlobalQuota,
    /// Tenant distinct-job limit reached.
    TenantQuota,
    /// Process-local job id exhausted.
    JobIdExhausted,
    /// Worker referenced a nonexistent job.
    UnknownJob(JobId),
}

impl fmt::Display for TranscodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSpec(field) => write!(formatter, "invalid transcode {field}"),
            Self::UpscaleNotAllowed => {
                formatter.write_str("transcode output cannot upscale source")
            }
            Self::InvalidTenant => formatter.write_str("invalid transcode tenant"),
            Self::GlobalQuota => formatter.write_str("global transcode quota reached"),
            Self::TenantQuota => formatter.write_str("tenant transcode quota reached"),
            Self::JobIdExhausted => formatter.write_str("transcode job id exhausted"),
            Self::UnknownJob(id) => write!(formatter, "unknown transcode job {id:?}"),
        }
    }
}

impl std::error::Error for TranscodeError {}

#[cfg(test)]
mod tests {
    use super::{
        Coordinator, MediaCodec, NegotiationDecision, NegotiationRequest, NetworkQuality, Quotas,
        SourceDescriptor, TranscodeError, TranscodeSpec, negotiate,
    };

    fn source() -> SourceDescriptor {
        SourceDescriptor {
            source_id: "track-1".to_owned(),
            codec: MediaCodec::Vp9,
            width: 1_920,
            height: 1_080,
            frames_per_second: 30,
        }
    }

    fn spec(codec: MediaCodec) -> TranscodeSpec {
        TranscodeSpec {
            source: source(),
            target_codec: codec,
            width: 1_280,
            height: 720,
            frames_per_second: 30,
            bitrate_bps: 1_500_000,
        }
    }

    #[test]
    fn reuses_identical_output_before_charging_quota() {
        let mut coordinator = Coordinator::new(Quotas {
            global_jobs: 2,
            jobs_per_tenant: 1,
        });
        let first = coordinator
            .acquire("tenant-a", spec(MediaCodec::H264))
            .expect("allocate");
        let reused = coordinator
            .acquire("tenant-a", spec(MediaCodec::H264))
            .expect("reuse");
        assert_eq!(reused.job_id, first.job_id);
        assert!(reused.reused);
        assert_eq!(
            coordinator.acquire("tenant-a", spec(MediaCodec::Vp8)),
            Err(TranscodeError::TenantQuota)
        );
        assert!(coordinator.release(first.job_id));
        assert_eq!(coordinator.active_jobs(), 1);
        assert!(coordinator.release(first.job_id));
        assert_eq!(coordinator.active_jobs(), 0);
    }

    #[test]
    fn negotiates_direct_transcode_and_weak_network_fallback() {
        let mut request = NegotiationRequest {
            source: source(),
            subscriber_codecs: vec![MediaCodec::Vp9, MediaCodec::H264],
            network_quality: NetworkQuality::Good,
            allow_transcoding: true,
            hls_fallback_url: Some("https://cdn/live/index.m3u8".to_owned()),
            target_width: 1_280,
            target_height: 720,
            target_frames_per_second: 30,
            target_bitrate_bps: 1_500_000,
        };
        assert_eq!(
            negotiate(&request),
            NegotiationDecision::DirectForward {
                codec: MediaCodec::Vp9
            }
        );
        request.subscriber_codecs = vec![MediaCodec::H264];
        assert!(matches!(
            negotiate(&request),
            NegotiationDecision::Transcode(_)
        ));
        request.network_quality = NetworkQuality::Critical;
        assert!(matches!(
            negotiate(&request),
            NegotiationDecision::HlsFallback { .. }
        ));
    }

    #[test]
    fn rejects_cross_media_transcoding() {
        let mut invalid = spec(MediaCodec::Opus);
        invalid.width = 0;
        invalid.height = 0;
        invalid.frames_per_second = 0;
        assert_eq!(
            invalid.validate(),
            Err(TranscodeError::InvalidSpec("media kind"))
        );
    }
}
