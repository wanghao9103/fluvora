use std::path::Path;

pub(super) async fn remove_temporary_file(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!(
            "failed to remove temporary worker file {}: {error}",
            path.display()
        );
    }
}
