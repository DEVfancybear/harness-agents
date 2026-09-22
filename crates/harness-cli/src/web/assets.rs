//! The static UI, embedded in the binary.
//!
//! The assets are `include_str!`-ed rather than read from disk or built by a
//! toolchain, so what the server hands out is byte-for-byte what was compiled
//! (`ADR-N10` D2). There is no development server, so there is also no way for
//! the served asset to be a different revision than the API beside it.

use bytes::Bytes;
use http::{Response, StatusCode, header};
use http_body_util::{BodyExt, Full, combinators::BoxBody};

/// The page, compiled in.
const INDEX: &str = include_str!("assets/index.html");
/// The script that talks to the API, compiled in.
const APP_JS: &str = include_str!("assets/app.js");
/// The stylesheet, compiled in.
const APP_CSS: &str = include_str!("assets/app.css");

type Body = BoxBody<Bytes, std::convert::Infallible>;

fn body(text: &'static str, content_type: &'static str) -> Response<Body> {
    let response = Response::new(
        Full::new(Bytes::from_static(text.as_bytes()))
            .map_err(|never| match never {})
            .boxed(),
    );
    let mut response = response;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(content_type),
    );
    response
}

/// Serve one asset path, or 404.
#[must_use]
pub fn static_response(path: &str) -> Response<Body> {
    match path {
        "/index.html" | "/" => body(INDEX, "text/html; charset=utf-8"),
        "/assets/app.js" => body(APP_JS, "text/javascript; charset=utf-8"),
        "/assets/app.css" => body(APP_CSS, "text/css; charset=utf-8"),
        _ => {
            let mut response = Response::new(
                Full::new(Bytes::from_static(b"not found"))
                    .map_err(|never| match never {})
                    .boxed(),
            );
            *response.status_mut() = StatusCode::NOT_FOUND;
            response
        }
    }
}
