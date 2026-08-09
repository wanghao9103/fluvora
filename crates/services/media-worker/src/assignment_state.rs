use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use axum::http::StatusCode;

use super::{ApiError, AssignmentRegistry, FenceSnapshot, validate_identifier};

const MAX_FENCE_SNAPSHOT_BYTES: u64 = 4 * 1_024 * 1_024;

pub(super) fn persist_fence_snapshot(
    directory: &Path,
    snapshot: &FenceSnapshot,
) -> Result<(), ApiError> {
    let bytes = serde_json::to_vec(snapshot).map_err(|error| {
        eprintln!("failed to serialize worker assignment snapshot: {error}");
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "assignment_state_invalid",
            message: "worker assignment state is invalid".to_owned(),
        }
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FENCE_SNAPSHOT_BYTES {
        return Err(ApiError {
            status: StatusCode::INSUFFICIENT_STORAGE,
            code: "assignment_state_too_large",
            message: "worker assignment fence state exceeds 4 MiB".to_owned(),
        });
    }
    let path = directory.join(format!("{:020}.json", snapshot.revision));
    if path.exists() {
        return match read_bounded_fence_snapshot(&path) {
            Ok(existing) if existing == bytes => Ok(()),
            Ok(_) | Err(_) => Err(ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "assignment_revision_conflict",
                message: "worker assignment revision already contains different data".to_owned(),
            }),
        };
    }
    let temporary = directory.join(format!(
        ".{:020}.{}.tmp",
        snapshot.revision,
        std::process::id()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|error| assignment_io_error(&temporary, error))?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        remove_temporary_snapshot(&temporary);
        return Err(assignment_io_error(&temporary, error));
    }
    drop(file);
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let identical_target = read_bounded_fence_snapshot(&path).is_ok_and(|value| value == bytes);
        remove_temporary_snapshot(&temporary);
        if !identical_target {
            return Err(assignment_io_error(&path, error));
        }
    }
    prune_fence_snapshots(directory, snapshot.revision);
    Ok(())
}

fn remove_temporary_snapshot(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!(
            "failed to remove temporary assignment snapshot {}: {error}",
            path.display()
        );
    }
}

pub(super) fn load_assignment_registry(directory: PathBuf) -> Result<AssignmentRegistry, String> {
    let mut snapshot = FenceSnapshot::default();
    let entries = std::fs::read_dir(&directory).map_err(|error| error.to_string())?;
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(error) => {
                eprintln!("ignoring unreadable worker assignment directory entry: {error}");
                continue;
            }
        };
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(file_revision) = fence_snapshot_revision(&path) else {
            eprintln!("ignoring malformed assignment snapshot {}", path.display());
            continue;
        };
        let bytes = match read_bounded_fence_snapshot(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!(
                    "ignoring unreadable assignment snapshot {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        let candidate = match serde_json::from_slice::<FenceSnapshot>(&bytes) {
            Ok(candidate) => candidate,
            Err(error) => {
                eprintln!(
                    "ignoring invalid assignment snapshot {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        if !valid_fence_snapshot(&candidate, file_revision) {
            eprintln!(
                "ignoring inconsistent assignment snapshot {}",
                path.display()
            );
            continue;
        }
        if candidate.revision > snapshot.revision {
            snapshot = candidate;
        }
    }
    Ok(AssignmentRegistry {
        snapshot,
        active: HashMap::new(),
        directory,
    })
}

fn read_bounded_fence_snapshot(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_FENCE_SNAPSHOT_BYTES {
        return Err("assignment snapshot is not a bounded regular file".to_owned());
    }
    std::fs::read(path).map_err(|error| error.to_string())
}

fn fence_snapshot_revision(path: &Path) -> Option<u64> {
    let stem = path.file_stem()?.to_str()?;
    (stem.len() == 20 && stem.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| stem.parse::<u64>().ok())
        .flatten()
        .filter(|revision| *revision > 0)
}

fn valid_fence_snapshot(snapshot: &FenceSnapshot, file_revision: u64) -> bool {
    snapshot.revision == file_revision
        && snapshot.records.len() <= 50_000
        && snapshot.records.iter().all(|(key, fence)| {
            let Some((kind, resource_id)) = key.split_once(':') else {
                return false;
            };
            matches!(kind, "vod" | "live" | "realtime")
                && validate_identifier(resource_id).is_ok()
                && validate_identifier(&fence.job_key).is_ok()
                && fence.generation > 0
        })
}

fn prune_fence_snapshots(directory: &Path, current_revision: u64) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut snapshots = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let revision = fence_snapshot_revision(&path)?;
            (revision <= current_revision).then_some((revision, path))
        })
        .collect::<Vec<_>>();
    snapshots.sort_unstable_by_key(|(revision, _)| std::cmp::Reverse(*revision));
    for (_, path) in snapshots.into_iter().skip(2) {
        if let Err(error) = std::fs::remove_file(&path) {
            eprintln!(
                "failed to prune worker assignment snapshot {}: {error}",
                path.display()
            );
        }
    }
}

fn assignment_io_error(path: &Path, error: std::io::Error) -> ApiError {
    eprintln!(
        "worker assignment snapshot I/O failed for {}: {error}",
        path.display()
    );
    drop(error);
    ApiError {
        status: StatusCode::INSUFFICIENT_STORAGE,
        code: "assignment_state_io_error",
        message: "worker assignment state could not be persisted".to_owned(),
    }
}
