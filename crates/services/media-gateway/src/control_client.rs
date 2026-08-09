use std::time::Duration;

use axum::http::StatusCode;
use serde::de::DeserializeOwned;

use super::ApiError;

const MAX_CONTROL_RESPONSE_BYTES: usize = 1_024 * 1_024;

pub(super) fn build_internal_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("build internal HTTP client")
}

pub(super) fn normalize_http_origin(value: &str) -> Result<String, &'static str> {
    if value.is_empty() || value.len() > 2_048 || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err("must contain 1..=2048 non-control bytes");
    }
    let parsed = reqwest::Url::parse(value).map_err(|_| "must be a valid absolute URL")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(parsed.path(), "" | "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("must be an HTTP(S) origin without credentials, path, query, or fragment");
    }
    Ok(parsed.as_str().trim_end_matches('/').to_owned())
}

pub(super) fn validate_control_token(value: &str) -> Result<(), &'static str> {
    if !(16..=4_096).contains(&value.len()) || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err("must contain 16..=4096 non-control bytes");
    }
    Ok(())
}

pub(super) fn internal_url(base_url: &str, path: &str) -> Result<reqwest::Url, ApiError> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.len() > 2_048
        || path.bytes().any(|byte| !byte.is_ascii_graphic())
        || path.contains(['\\', '?', '#'])
    {
        return Err(invalid_internal_endpoint(
            "internal control path must be a bounded absolute path",
        ));
    }
    let origin = normalize_http_origin(base_url).map_err(invalid_internal_endpoint)?;
    reqwest::Url::parse(&format!("{origin}{path}"))
        .map_err(|_| invalid_internal_endpoint("internal control URL is invalid"))
}

pub(super) async fn bounded_json<T>(
    mut response: reqwest::Response,
    invalid_response_code: &'static str,
) -> Result<T, ApiError>
where
    T: DeserializeOwned,
{
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CONTROL_RESPONSE_BYTES as u64)
    {
        return Err(invalid_response(
            invalid_response_code,
            "internal service response exceeds 1 MiB",
        ));
    }
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .filter(|value| {
            value == "application/json"
                || (value.starts_with("application/") && value.ends_with("+json"))
        })
        .ok_or_else(|| {
            invalid_response(
                invalid_response_code,
                "internal service response must contain JSON",
            )
        })?;

    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(MAX_CONTROL_RESPONSE_BYTES),
    );
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        invalid_response(
            invalid_response_code,
            "failed to read internal service response",
        )
    })? {
        if chunk.len() > MAX_CONTROL_RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err(invalid_response(
                invalid_response_code,
                "internal service response exceeds 1 MiB",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        invalid_response(
            invalid_response_code,
            "internal service returned malformed JSON",
        )
    })
}

fn invalid_internal_endpoint(message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::BAD_GATEWAY,
        code: "invalid_internal_endpoint",
        message: message.into(),
    }
}

fn invalid_response(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::BAD_GATEWAY,
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::response::Redirect;
    use axum::routing::get;

    use super::{
        bounded_json, build_internal_http_client, internal_url, normalize_http_origin,
        validate_control_token,
    };

    #[test]
    fn validates_internal_origins_paths_and_tokens() {
        assert_eq!(
            normalize_http_origin("HTTPS://worker.example:8443/").expect("origin"),
            "https://worker.example:8443"
        );
        assert!(normalize_http_origin("http://token@worker.example").is_err());
        assert!(normalize_http_origin("http://worker.example/base").is_err());
        assert!(normalize_http_origin("file:///tmp/worker").is_err());
        assert!(internal_url("https://worker.example", "//attacker.example/path").is_err());
        assert!(internal_url("https://worker.example", "/path?redirect=true").is_err());
        assert!(internal_url("https://worker.example", "/path\\other").is_err());
        assert!(validate_control_token("0123456789abcdef").is_ok());
        assert!(validate_control_token("too-short").is_err());
        assert!(validate_control_token("0123456789abcde\n").is_err());
    }

    #[tokio::test]
    async fn disables_redirects_and_bounds_json_responses() {
        let app = Router::new()
            .route(
                "/redirect",
                get(|| async { Redirect::temporary("/target") }),
            )
            .route(
                "/target",
                get(|| async { axum::Json(serde_json::json!({"ok": true})) }),
            )
            .route(
                "/large",
                get(|| async {
                    axum::Json(serde_json::json!({
                        "payload": "a".repeat(1_024 * 1_024)
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("test server");
        });
        let client = build_internal_http_client();

        let redirect = client
            .get(format!("http://{address}/redirect"))
            .send()
            .await
            .expect("redirect response");
        assert!(redirect.status().is_redirection());
        let large = client
            .get(format!("http://{address}/large"))
            .send()
            .await
            .expect("large response");
        let error = bounded_json::<serde_json::Value>(large, "worker_invalid_response")
            .await
            .expect_err("bounded response");
        assert_eq!(error.code, "worker_invalid_response");
        server.abort();
    }
}
