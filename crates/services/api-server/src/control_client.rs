use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use fluvora_control_store::ServicePlacement;
use fluvora_domain::RoomId;
use fluvora_status_service::ServiceKind;
use serde::{Deserialize, Serialize};

use crate::config::normalize_control_url;
use crate::error::ApiError;
use crate::models::AppState;
use crate::persistence::RoomPersistence;
use crate::runtime::format_id;

pub(super) const MAX_CONTROL_RESPONSE_BYTES: usize = 1_024 * 1_024;

pub(super) fn build_internal_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("build internal HTTP client")
}

pub(super) async fn bounded_response_bytes(
    mut response: reqwest::Response,
    too_large_code: &'static str,
    invalid_response_code: &'static str,
) -> Result<Vec<u8>, ApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CONTROL_RESPONSE_BYTES as u64)
    {
        return Err(rejected(
            too_large_code,
            "internal service response exceeds 1 MiB",
        ));
    }
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(MAX_CONTROL_RESPONSE_BYTES),
    );
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        rejected(
            invalid_response_code,
            format!("failed to read internal service response: {error}"),
        )
    })? {
        if chunk.len() > MAX_CONTROL_RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err(rejected(
                too_large_code,
                "internal service response exceeds 1 MiB",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(super) async fn delete_media_session(
    state: &AppState,
    room_id: RoomId,
    session_id: u64,
) -> Result<(), ApiError> {
    let endpoint = media_control_endpoint(state, room_id).await?;
    let path = format!("/v1/sessions/{session_id}");
    let response = state
        .http_client
        .delete(internal_url(&endpoint, &path)?)
        .bearer_auth(state.media_control_token.as_ref())
        .send()
        .await
        .map_err(|error| unavailable("media_node_unavailable", &error))?;
    if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
        Ok(())
    } else {
        Err(rejected(
            "media_session_delete_failed",
            format!("media node returned {}", response.status()),
        ))
    }
}

pub(super) async fn media_control_post(
    state: &AppState,
    room_id: RoomId,
    path: &str,
    body: &impl Serialize,
) -> Result<(), ApiError> {
    let endpoint = media_control_endpoint(state, room_id).await?;
    let response = state
        .http_client
        .post(internal_url(&endpoint, path)?)
        .bearer_auth(state.media_control_token.as_ref())
        .json(body)
        .send()
        .await
        .map_err(|error| unavailable("media_node_unavailable", &error))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(rejected(
            "media_control_rejected",
            format!("media node returned {}", response.status()),
        ))
    }
}

pub(super) async fn media_control_json_post<Request, Output>(
    state: &AppState,
    room_id: RoomId,
    path: &str,
    body: &Request,
) -> Result<Output, ApiError>
where
    Request: Serialize + ?Sized,
    Output: for<'de> Deserialize<'de>,
{
    let endpoint = media_control_endpoint(state, room_id).await?;
    internal_json_post(
        &state.http_client,
        &endpoint,
        &state.media_control_token,
        path,
        body,
        "media_node_unavailable",
        "media_control_rejected",
    )
    .await
}

pub(super) async fn worker_control_json_post<Request, Output>(
    state: &AppState,
    endpoint: &str,
    path: &str,
    body: &Request,
) -> Result<Output, ApiError>
where
    Request: Serialize + ?Sized,
    Output: for<'de> Deserialize<'de>,
{
    internal_json_post(
        &state.http_client,
        endpoint,
        &state.worker_control_token,
        path,
        body,
        "media_worker_unavailable",
        "media_worker_rejected",
    )
    .await
}

pub(super) async fn media_control_delete_json(
    state: &AppState,
    room_id: RoomId,
    path: &str,
    body: &impl Serialize,
) -> Result<(), ApiError> {
    let endpoint = media_control_endpoint(state, room_id).await?;
    let response = state
        .http_client
        .delete(internal_url(&endpoint, path)?)
        .bearer_auth(state.media_control_token.as_ref())
        .json(body)
        .send()
        .await
        .map_err(|error| unavailable("media_node_unavailable", &error))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(rejected(
            "media_control_rejected",
            format!("media node returned {}", response.status()),
        ))
    }
}

pub(super) async fn media_control_endpoint(
    state: &AppState,
    room_id: RoomId,
) -> Result<Arc<str>, ApiError> {
    match state.persistence.as_ref() {
        RoomPersistence::Files(_) => Ok(state.media_control_url.clone()),
        RoomPersistence::Postgres(store) => {
            let placement = store
                .place_room(
                    &format_id(room_id.0),
                    &state.region,
                    state.placement_stale_after,
                )
                .await
                .map_err(ApiError::from)?;
            Ok(Arc::from(validated_endpoint(&placement.endpoint)?))
        }
    }
}

pub(super) async fn media_candidate(
    state: &AppState,
    room_id: RoomId,
) -> Result<Option<String>, ApiError> {
    match state.persistence.as_ref() {
        RoomPersistence::Files(_) => Ok(state.candidate.as_deref().map(str::to_owned)),
        RoomPersistence::Postgres(store) => store
            .place_room(
                &format_id(room_id.0),
                &state.region,
                state.placement_stale_after,
            )
            .await
            .map(|placement| placement.ice_candidate)
            .map_err(ApiError::from),
    }
}

pub(super) async fn worker_control_placement(
    state: &AppState,
    resource_id: &str,
) -> Result<ServicePlacement, ApiError> {
    match state.persistence.as_ref() {
        RoomPersistence::Files(_) => Ok(ServicePlacement {
            resource_kind: "realtime_job".to_owned(),
            resource_id: resource_id.to_owned(),
            node_id: "static-worker".to_owned(),
            endpoint: state.worker_control_url.to_string(),
            generation: 1,
        }),
        RoomPersistence::Postgres(store) => {
            let placement = store
                .place_service_resource(
                    "realtime_job",
                    resource_id,
                    ServiceKind::MediaWorker.as_str(),
                    &state.region,
                    state.placement_stale_after,
                )
                .await
                .map_err(ApiError::from)?;
            validate_placement(placement)
        }
    }
}

pub(super) async fn advance_worker_placement(
    state: &AppState,
    resource_id: &str,
    current_generation: u64,
) -> Result<ServicePlacement, ApiError> {
    match state.persistence.as_ref() {
        RoomPersistence::Files(_) => {
            let mut placement = worker_control_placement(state, resource_id).await?;
            placement.generation = current_generation.saturating_add(1);
            Ok(placement)
        }
        RoomPersistence::Postgres(store) => {
            let placement = store
                .advance_service_placement(
                    "realtime_job",
                    resource_id,
                    ServiceKind::MediaWorker.as_str(),
                    &state.region,
                    state.placement_stale_after,
                )
                .await
                .map_err(ApiError::from)?;
            validate_placement(placement)
        }
    }
}

pub(super) async fn remove_worker_placement(state: &AppState, resource_id: &str) {
    if let RoomPersistence::Postgres(store) = state.persistence.as_ref()
        && let Err(error) = store
            .remove_service_placement("realtime_job", resource_id)
            .await
    {
        eprintln!("failed to release worker placement {resource_id}: {error}");
    }
}

pub(super) async fn remove_worker_placement_generation(
    state: &AppState,
    resource_id: &str,
    generation: u64,
) {
    if let RoomPersistence::Postgres(store) = state.persistence.as_ref()
        && let Err(error) = store
            .remove_service_placement_generation("realtime_job", resource_id, generation)
            .await
    {
        eprintln!(
            "failed to release worker placement {resource_id} generation {generation}: {error}"
        );
    }
}

pub(super) async fn media_control_internal_delete(state: &AppState, room_id: RoomId, path: &str) {
    match media_control_endpoint(state, room_id).await {
        Ok(endpoint) => {
            internal_delete(state, &endpoint, &state.media_control_token, path).await;
        }
        Err(error) => {
            eprintln!(
                "failed to resolve media control endpoint for cleanup {}: {}",
                format_id(room_id.0),
                error.message
            );
        }
    }
}

pub(super) async fn internal_delete(state: &AppState, base_url: &str, token: &str, path: &str) {
    let url = match internal_url(base_url, path) {
        Ok(url) => url,
        Err(error) => {
            eprintln!(
                "refusing invalid internal cleanup endpoint: {}",
                error.message
            );
            return;
        }
    };
    match state
        .http_client
        .delete(url)
        .bearer_auth(token)
        .send()
        .await
    {
        Ok(response)
            if response.status().is_success()
                || response.status() == reqwest::StatusCode::NOT_FOUND => {}
        Ok(response) => {
            eprintln!(
                "internal cleanup request was rejected with {} for {path}",
                response.status()
            );
        }
        Err(error) => {
            eprintln!("internal cleanup request failed for {path}: {error}");
        }
    }
}

async fn internal_json_post<Request, Output>(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    path: &str,
    body: &Request,
    unavailable_code: &'static str,
    rejected_code: &'static str,
) -> Result<Output, ApiError>
where
    Request: Serialize + ?Sized,
    Output: for<'de> Deserialize<'de>,
{
    let response = client
        .post(internal_url(base_url, path)?)
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .map_err(|error| unavailable(unavailable_code, &error))?;
    if !response.status().is_success() {
        return Err(rejected(
            rejected_code,
            format!("internal service returned {}", response.status()),
        ));
    }
    let bytes = bounded_response_bytes(response, rejected_code, rejected_code).await?;
    serde_json::from_slice(&bytes).map_err(|error| {
        rejected(
            rejected_code,
            format!("internal service returned invalid JSON: {error}"),
        )
    })
}

pub(super) fn internal_url(base_url: &str, path: &str) -> Result<reqwest::Url, ApiError> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.len() > 2_048
        || path.bytes().any(|byte| !byte.is_ascii_graphic())
        || path.contains(['?', '#'])
    {
        return Err(invalid_endpoint(
            "internal control path must be a bounded absolute path",
        ));
    }
    let normalized = validated_endpoint(base_url)?;
    let mut url = reqwest::Url::parse(&normalized)
        .map_err(|_| invalid_endpoint("internal control origin is invalid"))?;
    url.set_path(path);
    Ok(url)
}

fn validate_placement(mut placement: ServicePlacement) -> Result<ServicePlacement, ApiError> {
    placement.endpoint = validated_endpoint(&placement.endpoint)?;
    Ok(placement)
}

fn validated_endpoint(endpoint: &str) -> Result<String, ApiError> {
    normalize_control_url(endpoint).map_err(invalid_endpoint)
}

fn invalid_endpoint(message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::BAD_GATEWAY,
        code: "invalid_internal_endpoint",
        message: message.into(),
    }
}

fn unavailable(code: &'static str, error: &reqwest::Error) -> ApiError {
    eprintln!("internal service request failed ({code}): {error}");
    ApiError {
        status: StatusCode::BAD_GATEWAY,
        code,
        message: "internal service is unavailable".to_owned(),
    }
}

fn rejected(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::BAD_GATEWAY,
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use axum::Router;
    use axum::body::{Body, Bytes};
    use axum::routing::get;
    use futures_util::stream;

    use super::{bounded_response_bytes, internal_url};

    #[test]
    fn joins_only_valid_control_origins_and_absolute_paths() {
        assert_eq!(
            internal_url("https://media.example:8443/", "/v1/sessions/42")
                .expect("internal URL")
                .as_str(),
            "https://media.example:8443/v1/sessions/42"
        );
        assert!(internal_url("http://token@media.example", "/v1/sessions").is_err());
        assert!(internal_url("http://media.example/base", "/v1/sessions").is_err());
        assert!(internal_url("http://media.example", "//attacker.example/path").is_err());
        assert!(internal_url("http://media.example", "/path?redirect=attacker").is_err());
        assert!(internal_url("file:///etc", "/passwd").is_err());
    }

    #[tokio::test]
    async fn bounds_chunked_internal_responses_while_streaming() {
        let app = Router::new().route(
            "/large",
            get(|| async {
                let chunks = stream::iter([
                    Ok::<_, Infallible>(Bytes::from(vec![b'a'; 600 * 1_024])),
                    Ok::<_, Infallible>(Bytes::from(vec![b'b'; 600 * 1_024])),
                ]);
                Body::from_stream(chunks)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });

        let response = reqwest::get(format!("http://{address}/large"))
            .await
            .expect("chunked response");
        assert!(response.content_length().is_none());
        let error = bounded_response_bytes(response, "too_large", "invalid")
            .await
            .expect_err("bounded response");
        assert_eq!(error.code, "too_large");
        server.abort();
    }
}
