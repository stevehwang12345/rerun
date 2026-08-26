use axum::{Json, http::StatusCode, response::Response};
use serde::Serialize;

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl ApiError {
    pub(crate) fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }

    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", message)
    }

    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "badRequest", message)
    }

    pub(crate) fn not_found(resource: &str, id: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "notFound",
            format!("{resource} `{id}` was not found."),
        )
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    pub(crate) fn payload_too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "payloadTooLarge", message)
    }

    pub(crate) fn unsupported_media_type(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupportedMediaType",
            message,
        )
    }

    pub(crate) fn gone(message: impl Into<String>) -> Self {
        Self::new(StatusCode::GONE, "gone", message)
    }

    pub(crate) fn request_timeout(message: impl Into<String>) -> Self {
        Self::new(StatusCode::REQUEST_TIMEOUT, "requestTimeout", message)
    }

    pub(crate) fn too_many_requests(message: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "tooManyRequests", message)
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internalError", message)
    }

    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;
