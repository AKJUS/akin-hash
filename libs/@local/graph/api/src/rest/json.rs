//! Vendored and modified from `axum::json::Json` to allow us to handle the rejection ourselves
//!
//! As recommended per <https://github.com/tokio-rs/axum/discussions/1610#discussioncomment-4292449>

use std::collections::HashMap;

use async_trait::async_trait;
use axum::{
    body::{Body, Bytes},
    extract::FromRequest,
    http::{HeaderMap, HeaderValue, Request, header},
    response::{IntoResponse, Response},
};
use bytes::{BufMut as _, BytesMut};
use hash_graph_type_defs::error::{ErrorInfo, Status, StatusPayloadInfo};
use hash_status::StatusCode;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::error::Category;

use crate::rest::status::status_to_response;

#[derive(Debug, Clone, Copy, Default)]
#[must_use]
pub(crate) struct Json<T>(pub T);

#[async_trait]
impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    // TODO: can we generally add `RequestInfo` including information such as the route
    //   see https://github.com/tokio-rs/axum/blob/main/examples/customize-extractor-error/src/custom_extractor.rs
    async fn from_request(req: Request<Body>, state: &S) -> Result<Self, Self::Rejection> {
        if json_content_type(req.headers()) {
            let bytes = Bytes::from_request(req, state).await.map_err(|err| {
                status_to_response(Status::new(
                    StatusCode::InvalidArgument,
                    Some("Failed to buffer body of request".to_owned()),
                    vec![StatusPayloadInfo::Error(ErrorInfo::new(
                        HashMap::from([(
                            "bufferError".to_owned(),
                            serde_json::to_value(err.to_string())
                                .expect("Failed to convert string to serde_json::Value"),
                        )]),
                        "UNKNOWN".to_owned(),
                    ))],
                ))
            })?;

            let value = match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(err) => {
                    let (message, reason) = match err.classify() {
                        Category::Io => (
                            "Failed to read request body bytes".to_owned(),
                            "UNKNOWN".to_owned(),
                        ),
                        Category::Syntax => (
                            "Incorrectly formatted JSON in request body".to_owned(),
                            "INVALID_JSON".to_owned(),
                        ),
                        Category::Data => (
                            "Unexpected JSON contents in request body".to_owned(),
                            "INVALID_CONTENTS".to_owned(),
                        ),
                        Category::Eof => (
                            "Unexpected end of JSON in request body".to_owned(),
                            "INVALID_JSON".to_owned(),
                        ),
                    };

                    return Err(status_to_response(Status::new(
                        StatusCode::InvalidArgument,
                        Some(message),
                        vec![StatusPayloadInfo::Error(ErrorInfo::new(
                            HashMap::from([(
                                "deserializationError".to_owned(),
                                serde_json::to_value(err.to_string())
                                    .expect("Failed to convert string to serde_json::Value"),
                            )]),
                            reason,
                        ))],
                    )));
                }
            };

            Ok(Self(value))
        } else {
            Err(status_to_response(Status::new(
                StatusCode::InvalidArgument,
                Some("Missing JSON content type in headers of request".to_owned()),
                vec![],
            )))
        }
    }
}

fn json_content_type(headers: &HeaderMap) -> bool {
    let Some(content_type) = headers.get(header::CONTENT_TYPE) else {
        return false;
    };

    let Ok(content_type) = content_type.to_str() else {
        return false;
    };

    let Ok(mime) = content_type.parse::<mime::Mime>() else {
        return false;
    };

    mime.type_() == "application"
        && (mime.subtype() == "json" || mime.suffix().is_some_and(|name| name == "json"))
}

impl<T> From<T> for Json<T> {
    fn from(inner: T) -> Self {
        Self(inner)
    }
}

impl<T> IntoResponse for Json<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        // Use a small initial capacity of 128 bytes like serde_json::to_vec
        // https://docs.rs/serde_json/1.0.82/src/serde_json/ser.rs.html#2189
        let mut buf = BytesMut::with_capacity(128).writer();
        match serde_json::to_writer(&mut buf, &self.0) {
            Ok(()) => (
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(mime::APPLICATION_JSON.as_ref()),
                )],
                buf.into_inner().freeze(),
            )
                .into_response(),
            Err(err) => status_to_response(Status::new(
                StatusCode::Internal,
                Some("Failed to serialize response".to_owned()),
                vec![StatusPayloadInfo::Error(ErrorInfo::new(
                    HashMap::from([(
                        "serializationError".to_owned(),
                        serde_json::to_value(err.to_string())
                            .expect("Failed to convert string to serde_json::Value"),
                    )]),
                    "UNKNOWN".to_owned(),
                ))],
            )),
        }
    }
}
