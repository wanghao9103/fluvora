use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use fluvora_media_pipeline::AssetState;
use serde::Serialize;
use tokio::io::AsyncWriteExt as _;

use super::control_client::normalize_http_origin;
use super::{
    ApiError, AppState, LiveStream, ManagedAsset, PersistedLiveStream, internal_error, io_error,
    validate_identifier, validate_rendition_ladder,
};

const MAX_METADATA_BYTES: u64 = 32 * 1_024 * 1_024;

pub(super) fn load_assets(metadata_root: &Path) -> Result<HashMap<String, ManagedAsset>, String> {
    let directory = metadata_root.join("assets");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let mut assets = HashMap::<String, ManagedAsset>::new();
    for path in json_files(&directory)? {
        let (file_id, file_revision) = match snapshot_identity(&path) {
            Ok(identity) => identity,
            Err(error) => {
                eprintln!(
                    "ignoring invalid asset snapshot {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        let bytes = match read_bounded_metadata(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!(
                    "ignoring unreadable asset snapshot {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        let mut managed = match serde_json::from_slice::<ManagedAsset>(&bytes) {
            Ok(managed) => managed,
            Err(error) => {
                eprintln!(
                    "ignoring invalid asset snapshot {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        if let Err(error) = validate_loaded_asset(&mut managed, &file_id, file_revision) {
            eprintln!(
                "ignoring inconsistent asset snapshot {}: {error}",
                path.display()
            );
            continue;
        }
        let replace = assets
            .get(&managed.asset.id)
            .is_none_or(|current| managed.revision > current.revision);
        if replace {
            assets.insert(managed.asset.id.clone(), managed);
        }
    }
    Ok(assets)
}

pub(super) fn load_live_streams(
    metadata_root: &Path,
) -> Result<HashMap<String, LiveStream>, String> {
    let directory = metadata_root.join("live");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let mut streams = HashMap::<String, LiveStream>::new();
    for path in json_files(&directory)? {
        let (file_id, file_revision) = match snapshot_identity(&path) {
            Ok(identity) => identity,
            Err(error) => {
                eprintln!("ignoring invalid live snapshot {}: {error}", path.display());
                continue;
            }
        };
        let bytes = match read_bounded_metadata(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!(
                    "ignoring unreadable live snapshot {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        let mut persisted = match serde_json::from_slice::<PersistedLiveStream>(&bytes) {
            Ok(persisted) => persisted,
            Err(error) => {
                eprintln!("ignoring invalid live snapshot {}: {error}", path.display());
                continue;
            }
        };
        if let Err(error) = validate_loaded_live(&mut persisted, &file_id, file_revision) {
            eprintln!(
                "ignoring inconsistent live snapshot {}: {error}",
                path.display()
            );
            continue;
        }
        let replace = streams
            .get(&persisted.stream_id)
            .is_none_or(|current| persisted.stream.revision > current.revision);
        if replace {
            streams.insert(persisted.stream_id, persisted.stream);
        }
    }
    Ok(streams)
}

pub(super) async fn persist_asset(
    state: &AppState,
    managed: &ManagedAsset,
) -> Result<(), ApiError> {
    let directory = state.metadata_root.join("assets");
    let path = directory.join(format!(
        "{}-{:020}.json",
        managed.asset.id, managed.revision
    ));
    persist_json(&path, managed).await?;
    prune_snapshots(&directory, &managed.asset.id, managed.revision);
    Ok(())
}

pub(super) async fn persist_live_stream(
    state: &AppState,
    stream_id: &str,
    stream: &LiveStream,
) -> Result<(), ApiError> {
    let directory = state.metadata_root.join("live");
    let path = directory.join(format!("{stream_id}-{:020}.json", stream.revision));
    persist_json(
        &path,
        &PersistedLiveStream {
            stream_id: stream_id.to_owned(),
            stream: stream.clone(),
        },
    )
    .await?;
    prune_snapshots(&directory, stream_id, stream.revision);
    Ok(())
}

fn validate_loaded_asset(
    managed: &mut ManagedAsset,
    file_id: &str,
    file_revision: u64,
) -> Result<(), String> {
    if managed.asset.id != file_id || managed.revision != file_revision || managed.revision == 0 {
        return Err("filename does not match internal asset identity".to_owned());
    }
    managed
        .asset
        .validate()
        .map_err(|error| error.to_string())?;
    if managed.created_at_millis > managed.updated_at_millis {
        return Err("asset timestamps are not monotonic".to_owned());
    }
    normalize_worker_endpoint(&mut managed.worker_endpoint)?;
    if managed.job_id.is_some()
        && (managed.worker_endpoint.is_none() || managed.placement_generation.is_none())
    {
        return Err("asset worker assignment is incomplete".to_owned());
    }
    if managed.worker_endpoint.is_some() != managed.placement_generation.is_some() {
        return Err("asset worker endpoint and generation must be paired".to_owned());
    }
    if let Some(spec) = &managed.job_spec {
        validate_segment_duration(spec.segment_duration_millis)?;
        validate_rendition_ladder(&spec.renditions, false).map_err(|error| error.message)?;
    }
    if matches!(managed.asset.state, AssetState::Transcoding { .. }) && managed.job_spec.is_none() {
        return Err("processed asset is missing its job specification".to_owned());
    }
    Ok(())
}

fn validate_loaded_live(
    persisted: &mut PersistedLiveStream,
    file_id: &str,
    file_revision: u64,
) -> Result<(), String> {
    if persisted.stream_id != file_id
        || persisted.stream.revision != file_revision
        || persisted.stream.revision == 0
        || validate_identifier(&persisted.stream_id).is_err()
    {
        return Err("filename does not match internal live identity".to_owned());
    }
    let stream = &mut persisted.stream;
    stream
        .playlist
        .validate()
        .map_err(|error| error.to_string())?;
    if stream.created_at_millis > stream.updated_at_millis
        || stream
            .finished_at_millis
            .is_some_and(|finished| finished < stream.created_at_millis)
        || stream
            .deleted_at_millis
            .is_some_and(|deleted| deleted < stream.created_at_millis)
        || stream
            .purged_at_millis
            .is_some_and(|purged| purged < stream.created_at_millis)
    {
        return Err("live timestamps are not monotonic".to_owned());
    }
    normalize_worker_endpoint(&mut stream.worker_endpoint)?;
    if stream.worker_active
        && (stream.worker_job_id.is_none()
            || stream.worker_endpoint.is_none()
            || stream.placement_generation.is_none())
    {
        return Err("active live worker assignment is incomplete".to_owned());
    }
    if stream.worker_endpoint.is_some() != stream.placement_generation.is_some() {
        return Err("live worker endpoint and generation must be paired".to_owned());
    }
    if stream.recording_bindings.len() > 2
        || stream
            .recording_bindings
            .iter()
            .any(|binding| validate_identifier(&binding.room_id).is_err())
    {
        return Err("live recording bindings are invalid".to_owned());
    }
    if let Some(spec) = &stream.job_spec {
        validate_segment_duration(spec.segment_duration_millis)?;
        if !(3..=10_000).contains(&spec.window_segments) || spec.tracks.len() > 2 {
            return Err("live job specification is outside supported bounds".to_owned());
        }
        validate_rendition_ladder(&spec.renditions, true).map_err(|error| error.message)?;
    }
    Ok(())
}

fn normalize_worker_endpoint(endpoint: &mut Option<String>) -> Result<(), String> {
    if let Some(value) = endpoint {
        *value = normalize_http_origin(value).map_err(str::to_owned)?;
    }
    Ok(())
}

fn validate_segment_duration(duration_millis: u32) -> Result<(), String> {
    if !(1_000..=10_000).contains(&duration_millis) {
        return Err("segment duration must be between 1000 and 10000 milliseconds".to_owned());
    }
    Ok(())
}

fn json_files(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(directory).map_err(|error| error.to_string())?;
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry)
                if entry.path().extension().and_then(|value| value.to_str()) == Some("json") =>
            {
                paths.push(entry.path());
            }
            Ok(_) => {}
            Err(error) => eprintln!(
                "ignoring unreadable metadata directory entry in {}: {error}",
                directory.display()
            ),
        }
    }
    Ok(paths)
}

fn snapshot_identity(path: &Path) -> Result<(String, u64), String> {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "snapshot filename is not UTF-8".to_owned())?;
    let (identifier, revision) = stem
        .rsplit_once('-')
        .ok_or_else(|| "snapshot filename has no revision".to_owned())?;
    if revision.len() != 20
        || !revision.bytes().all(|byte| byte.is_ascii_digit())
        || validate_identifier(identifier).is_err()
    {
        return Err("snapshot filename is malformed".to_owned());
    }
    let revision = revision
        .parse::<u64>()
        .map_err(|_| "snapshot revision is invalid".to_owned())?;
    if revision == 0 {
        return Err("snapshot revision must be positive".to_owned());
    }
    Ok((identifier.to_owned(), revision))
}

fn read_bounded_metadata(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_METADATA_BYTES {
        return Err("metadata file is not a bounded regular file".to_owned());
    }
    std::fs::read(path).map_err(|error| error.to_string())
}

async fn persist_json(path: &Path, value: &impl Serialize) -> Result<(), ApiError> {
    let bytes = serde_json::to_vec(value).map_err(internal_error)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_METADATA_BYTES {
        return Err(ApiError {
            status: StatusCode::INSUFFICIENT_STORAGE,
            code: "metadata_too_large",
            message: "media metadata snapshot exceeds 32 MiB".to_owned(),
        });
    }
    if let Ok(existing) = read_bounded_metadata(path) {
        return if existing == bytes {
            Ok(())
        } else {
            Err(ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "metadata_revision_conflict",
                message: "media metadata revision already contains different data".to_owned(),
            })
        };
    }

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| internal_error("metadata filename is invalid"))?;
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let write_result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, path).await
    }
    .await;
    if let Err(error) = write_result {
        let identical_target = tokio::fs::read(path)
            .await
            .is_ok_and(|existing| existing == bytes);
        remove_temporary_metadata(&temporary).await;
        if identical_target {
            return Ok(());
        }
        return Err(io_error(error));
    }
    Ok(())
}

async fn remove_temporary_metadata(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!(
            "failed to remove temporary media metadata {}: {error}",
            path.display()
        );
    }
}

fn prune_snapshots(directory: &Path, identifier: &str, current_revision: u64) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut snapshots = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let (snapshot_id, revision) = snapshot_identity(&path).ok()?;
            (snapshot_id == identifier && revision <= current_revision).then_some((revision, path))
        })
        .collect::<Vec<_>>();
    snapshots.sort_unstable_by_key(|(revision, _)| std::cmp::Reverse(*revision));
    for (_, path) in snapshots.into_iter().skip(2) {
        if let Err(error) = std::fs::remove_file(&path) {
            eprintln!("failed to prune media snapshot {}: {error}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use fluvora_media_pipeline::VodAsset;

    use super::{load_assets, persist_json};
    use crate::ManagedAsset;

    fn managed_asset(revision: u64) -> ManagedAsset {
        ManagedAsset {
            asset: VodAsset::create("asset-a", "tenant-a").expect("asset"),
            job_id: None,
            revision,
            created_at_millis: 1,
            updated_at_millis: revision,
            worker_endpoint: None,
            placement_generation: None,
            job_spec: None,
        }
    }

    #[tokio::test]
    async fn writes_snapshots_atomically_and_retries_identical_revisions() {
        let directory = tempfile::tempdir().expect("metadata directory");
        let path = directory.path().join("asset-a-00000000000000000001.json");
        let snapshot = managed_asset(1);
        persist_json(&path, &snapshot).await.expect("first write");
        persist_json(&path, &snapshot)
            .await
            .expect("idempotent retry");

        let conflicting = managed_asset(2);
        let error = persist_json(&path, &conflicting)
            .await
            .expect_err("conflicting revision");
        assert_eq!(error.code, "metadata_revision_conflict");
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("entries")
                .count(),
            1
        );
    }

    #[test]
    fn restores_previous_valid_snapshot_and_skips_forged_or_oversized_files() {
        let directory = tempfile::tempdir().expect("metadata directory");
        let assets = directory.path().join("assets");
        std::fs::create_dir_all(&assets).expect("assets directory");
        std::fs::write(
            assets.join("asset-a-00000000000000000001.json"),
            serde_json::to_vec(&managed_asset(1)).expect("snapshot"),
        )
        .expect("valid snapshot");
        std::fs::write(
            assets.join("asset-a-00000000000000000002.json"),
            b"{not-json",
        )
        .expect("corrupt snapshot");
        std::fs::write(
            assets.join("forged-00000000000000000003.json"),
            serde_json::to_vec(&managed_asset(3)).expect("forged snapshot"),
        )
        .expect("forged snapshot");
        let oversized = std::fs::File::create(assets.join("asset-a-00000000000000000004.json"))
            .expect("oversized snapshot");
        oversized
            .set_len(33 * 1_024 * 1_024)
            .expect("sparse oversized snapshot");

        let loaded = load_assets(directory.path()).expect("load snapshots");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["asset-a"].revision, 1);
    }
}
