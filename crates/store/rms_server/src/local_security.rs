use axum::{
    Json,
    extract::Request,
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{
            ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS,
            ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
            ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE, ACCESS_CONTROL_REQUEST_METHOD,
            HOST, ORIGIN, VARY,
        },
    },
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use serde_json::json;

const DEFAULT_ORIGINS: &[&str] = &["http://127.0.0.1:4173", "http://localhost:4173"];

pub(crate) async fn validate_request(request: Request, next: Next) -> Response {
    if !host_is_allowed(request.headers()) {
        return forbidden("The Host header is not allowed by the local RMS server.");
    }

    let Ok(origin) = allowed_origin(request.headers()) else {
        return forbidden("The Origin header is not allowed by the local RMS server.");
    };
    if request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key(ACCESS_CONTROL_REQUEST_METHOD)
    {
        let Some(origin) = origin else {
            return forbidden("CORS preflight requires an allowed Origin header.");
        };
        let mut response = StatusCode::NO_CONTENT.into_response();
        add_cors_headers(response.headers_mut(), origin);
        response.headers_mut().insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
        );
        response.headers_mut().insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Content-Type, Idempotency-Key, X-RMS-Request-ID"),
        );
        response
            .headers_mut()
            .insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
        return response;
    }

    let mut response = next.run(request).await;
    if let Some(origin) = origin {
        add_cors_headers(response.headers_mut(), origin);
    }
    response
}

fn host_is_allowed(headers: &HeaderMap) -> bool {
    let Some(raw_host) = headers.get(HOST).and_then(|host| host.to_str().ok()) else {
        return false;
    };
    let host = raw_host.trim().to_ascii_lowercase();
    if configured_values("RMS_ALLOWED_HOSTS").any(|configured| configured == host) {
        return true;
    }
    host.parse::<axum::http::uri::Authority>()
        .ok()
        .is_some_and(|authority| {
            matches!(
                authority.host(),
                "127.0.0.1" | "localhost" | "::1" | "[::1]"
            )
        })
}

fn allowed_origin(headers: &HeaderMap) -> Result<Option<HeaderValue>, ()> {
    let Some(origin) = headers.get(ORIGIN) else {
        return Ok(None);
    };
    let origin_text = origin.to_str().map_err(|_invalid_header| ())?;
    let is_default = DEFAULT_ORIGINS.contains(&origin_text);
    let is_configured =
        configured_values("RMS_ALLOWED_ORIGINS").any(|configured| configured == origin_text);
    (is_default || is_configured)
        .then(|| origin.clone())
        .map(Some)
        .ok_or(())
}

fn configured_values(variable: &str) -> impl Iterator<Item = String> {
    std::env::var(variable)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .into_iter()
}

fn add_cors_headers(headers: &mut HeaderMap, origin: HeaderValue) {
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.insert(
        ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    headers.insert(
        ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Accept-Ranges, Content-Length, Content-Range, ETag"),
    );
    headers.insert(VARY, HeaderValue::from_static("Origin"));
}

fn forbidden(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "code": "forbidden", "message": message })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header::HOST};

    use super::host_is_allowed;

    #[test]
    fn loopback_hosts_allow_any_port_but_reject_rebinding_names() {
        for host in ["127.0.0.1:8080", "localhost:4173", "[::1]:8080"] {
            let mut headers = HeaderMap::new();
            headers.insert(HOST, HeaderValue::from_str(host).unwrap());
            assert!(host_is_allowed(&headers), "{host}");
        }
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("attacker.example:8080"));
        assert!(!host_is_allowed(&headers));
    }
}
