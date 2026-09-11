//! The admin routes' reply vocabulary — the one piece of the app skeleton every subsystem's
//! routes speak, extracted so the plugin crates can answer in the Node build's shape without
//! depending on the composition crate. `{error: "..."}` for failures, bare JSON for success,
//! and the body extractor the Node router's readJsonBody behaved like.

use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

/// Max JSON request body accepted — `BODY_LIMIT` in the Node router. Enforced once, on the
/// router, for every route (axum's DefaultBodyLimit).
pub const BODY_LIMIT: usize = 2 * 1024 * 1024;

/// An admin-route error: `{error}`.
pub fn admin_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

pub fn admin_json(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// The request body the Node router handed every handler: parsed JSON, or nothing when the client
/// sent no bytes (`undefined` there, [`Value::Null`] here — `Value::get` reads both the same).
/// A body that IS present but does not parse is refused before the handler runs, in Node's shape:
/// `readJsonBody` rejected `{error: "invalid JSON body: …"}` with a 400, and a body over
/// BODY_LIMIT rejected 413 `request body exceeds N bytes`.
///
/// Unlike axum's `Json` extractor this ignores Content-Type — Node parsed the bytes whatever
/// header the client sent (or none), and the panel sometimes posts without it.
pub struct NodeBody(pub Value);

pub struct NodeBodyRejection(Response);

impl IntoResponse for NodeBodyRejection {
    fn into_response(self) -> Response {
        self.0
    }
}

impl<S: Send + Sync> axum::extract::FromRequest<S> for NodeBody {
    type Rejection = NodeBodyRejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|_| {
                // Bytes::from_request fails for one reason: the router's body limit.
                NodeBodyRejection(admin_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!("request body exceeds {BODY_LIMIT} bytes"),
                ))
            })?;
        if bytes.is_empty() {
            return Ok(NodeBody(Value::Null));
        }
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => Ok(NodeBody(value)),
            Err(err) => Err(NodeBodyRejection(admin_error(
                StatusCode::BAD_REQUEST,
                &format!("invalid JSON body: {err}"),
            ))),
        }
    }
}
