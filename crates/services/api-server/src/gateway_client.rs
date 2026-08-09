use axum::body::{Body, Bytes};
use axum::http::{StatusCode, header::CONTENT_TYPE};
use axum::response::Response;

use crate::config::normalize_control_url;
use crate::control_client::bounded_response_bytes;
use crate::error::{ApiError, internal_error};
use crate::models::AppState;

const MAX_GATEWAY_PATH_BYTES: usize = 4_096;
const JSON_CONTENT_TYPE: &str = "application/json";

pub(super) async fn gateway_json_request(
    state: &AppState,
    method: reqwest::Method,
    path: &str,
    value: &serde_json::Value,
) -> Result<Response, ApiError> {
    let body = Bytes::from(serde_json::to_vec(value).map_err(internal_error)?);
    gateway_request(state, method, path, Some((body, JSON_CONTENT_TYPE))).await
}

pub(super) async fn gateway_request(
    state: &AppState,
    method: reqwest::Method,
    path: &str,
    body: Option<(Bytes, &'static str)>,
) -> Result<Response, ApiError> {
    gateway_exchange(
        &state.http_client,
        &state.gateway_control_url,
        &state.gateway_control_token,
        method,
        path,
        body,
    )
    .await
}

async fn gateway_exchange(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<(Bytes, &'static str)>,
) -> Result<Response, ApiError> {
    validate_gateway_request(
        &method,
        body.as_ref().map(|(_, content_type)| *content_type),
    )?;
    let url = gateway_url(base_url, path)?;
    let mut request = client
        .request(method, url)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, JSON_CONTENT_TYPE);
    if let Some((body, content_type)) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body);
    }
    let response = request.send().await.map_err(|error| {
        eprintln!("media gateway request failed for {path}: {error}");
        ApiError {
            status: StatusCode::BAD_GATEWAY,
            code: "media_gateway_unavailable",
            message: "media gateway is unavailable".to_owned(),
        }
    })?;
    let status = proxy_status(response.status())?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .cloned();
    let bytes = bounded_response_bytes(
        response,
        "media_gateway_response_too_large",
        "media_gateway_invalid_response",
    )
    .await?;
    let response_content_type = validate_gateway_body(content_type.as_ref(), &bytes)?;
    let mut response = Response::builder().status(status);
    if let Some(content_type) = response_content_type {
        response = response.header(CONTENT_TYPE, content_type);
    }
    response.body(Body::from(bytes)).map_err(internal_error)
}

fn gateway_url(base_url: &str, path: &str) -> Result<reqwest::Url, ApiError> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.len() > MAX_GATEWAY_PATH_BYTES
        || path.bytes().any(|byte| !byte.is_ascii_graphic())
        || path.contains(['\\', '#'])
    {
        return Err(invalid_gateway_endpoint(
            "media gateway path must be a bounded absolute path",
        ));
    }
    let normalized = normalize_control_url(base_url).map_err(invalid_gateway_endpoint)?;
    reqwest::Url::parse(&format!("{normalized}{path}"))
        .map_err(|_| invalid_gateway_endpoint("media gateway URL is invalid"))
}

fn validate_gateway_request(
    method: &reqwest::Method,
    content_type: Option<&str>,
) -> Result<(), ApiError> {
    if ![
        reqwest::Method::GET,
        reqwest::Method::POST,
        reqwest::Method::PUT,
        reqwest::Method::PATCH,
        reqwest::Method::DELETE,
    ]
    .contains(method)
    {
        return Err(invalid_gateway_request(
            "unsupported media gateway request method",
        ));
    }
    if content_type.is_some_and(|content_type| {
        !matches!(
            content_type,
            JSON_CONTENT_TYPE | "application/octet-stream" | "video/mp4" | "video/iso.segment"
        )
    }) {
        return Err(invalid_gateway_request(
            "unsupported media gateway request content type",
        ));
    }
    Ok(())
}

fn proxy_status(status: reqwest::StatusCode) -> Result<StatusCode, ApiError> {
    if !(status.is_success() || status.is_client_error()) {
        return Err(ApiError {
            status: StatusCode::BAD_GATEWAY,
            code: "media_gateway_invalid_response",
            message: format!("media gateway returned unexpected status {status}"),
        });
    }
    StatusCode::from_u16(status.as_u16()).map_err(internal_error)
}

fn validate_gateway_body(
    content_type: Option<&reqwest::header::HeaderValue>,
    bytes: &[u8],
) -> Result<Option<&'static str>, ApiError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    content_type
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| {
            let value = value.to_ascii_lowercase();
            value == JSON_CONTENT_TYPE
                || (value.starts_with("application/") && value.ends_with("+json"))
        })
        .ok_or_else(|| invalid_gateway_response("media gateway response must contain JSON"))?;
    serde_json::from_slice::<serde_json::Value>(bytes)
        .map_err(|_| invalid_gateway_response("media gateway returned malformed JSON"))?;
    Ok(Some(JSON_CONTENT_TYPE))
}

fn invalid_gateway_endpoint(message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::BAD_GATEWAY,
        code: "media_gateway_invalid_endpoint",
        message: message.into(),
    }
}

fn invalid_gateway_request(message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "media_gateway_invalid_request",
        message: message.into(),
    }
}

fn invalid_gateway_response(message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::BAD_GATEWAY,
        code: "media_gateway_invalid_response",
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::response::Redirect;
    use axum::routing::get;

    use super::{
        gateway_exchange, gateway_url, proxy_status, validate_gateway_body,
        validate_gateway_request,
    };
    use crate::control_client::build_internal_http_client;

    #[test]
    fn accepts_only_bounded_gateway_paths_and_control_methods() {
        assert_eq!(
            gateway_url(
                "https://gateway.example:8443/",
                "/v1/live/stream/segments/1?duration_millis=2000"
            )
            .expect("gateway URL")
            .as_str(),
            "https://gateway.example:8443/v1/live/stream/segments/1?duration_millis=2000"
        );
        assert!(gateway_url("https://gateway.example", "//attacker.example/path").is_err());
        assert!(gateway_url("https://gateway.example", "/path\\attacker").is_err());
        assert!(gateway_url("https://gateway.example", "/path#fragment").is_err());
        assert!(gateway_url("http://token@gateway.example", "/v1/assets").is_err());
        assert!(
            validate_gateway_request(&reqwest::Method::POST, Some("application/octet-stream"))
                .is_ok()
        );
        assert!(validate_gateway_request(&reqwest::Method::TRACE, None).is_err());
        assert!(validate_gateway_request(&reqwest::Method::POST, Some("text/plain")).is_err());
    }

    #[test]
    fn accepts_only_success_or_client_error_json_responses() {
        assert_eq!(
            proxy_status(reqwest::StatusCode::CREATED).expect("created status"),
            axum::http::StatusCode::CREATED
        );
        assert_eq!(
            proxy_status(reqwest::StatusCode::NOT_FOUND).expect("not found status"),
            axum::http::StatusCode::NOT_FOUND
        );
        assert!(proxy_status(reqwest::StatusCode::TEMPORARY_REDIRECT).is_err());
        assert!(proxy_status(reqwest::StatusCode::SERVICE_UNAVAILABLE).is_err());

        let json =
            reqwest::header::HeaderValue::from_static("application/problem+json; charset=utf-8");
        assert_eq!(
            validate_gateway_body(Some(&json), br#"{"code":"not_found"}"#).expect("JSON response"),
            Some("application/json")
        );
        let html = reqwest::header::HeaderValue::from_static("text/html");
        assert!(validate_gateway_body(Some(&html), b"<h1>error</h1>").is_err());
        assert!(validate_gateway_body(Some(&json), b"not-json").is_err());
        assert_eq!(
            validate_gateway_body(None, b"").expect("empty response"),
            None
        );
    }

    #[tokio::test]
    async fn internal_client_does_not_follow_gateway_redirects() {
        let app = Router::new()
            .route(
                "/redirect",
                get(|| async { Redirect::temporary("/target") }),
            )
            .route(
                "/target",
                get(|| async { axum::Json(serde_json::json!({"ok": true})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });

        let error = gateway_exchange(
            &build_internal_http_client(),
            &format!("http://{address}"),
            "test-token",
            reqwest::Method::GET,
            "/redirect",
            None,
        )
        .await
        .expect_err("redirect must not be followed");
        assert_eq!(error.code, "media_gateway_invalid_response");
        server.abort();
    }
}
