//! The `oma-media` protocol (M5 spec §6): the Tauri-free core of the
//! custom URI scheme that serves attachment media to the viewer.
//!
//! It serves only:
//! - **images the app produced** (`thumb`, 640 px; `display`, 2048 px),
//!   decoded and re-encoded in a helper process ([`helper`]);
//! - **the original bytes of audio and video** (`stream`), in ranges, only
//!   after the full hash check (integrity only), the structural check and
//!   the user's confirmation enabled the file in this session.
//!
//! `lib.rs` registers the scheme with `register_asynchronous_uri_scheme_protocol`
//! and turns the Tauri request into a [`MediaRequest`] and the responder
//! into a [`Reply`]; everything else is here, tested without a window.
//!
//! **The handler never blocks** (on WebKitGTK it runs on the GTK main
//! loop): [`dispatch`] parses, authorises, answers cache hits and refusals
//! at once, and hands everything else to the session's executors
//! ([`engine`]).
//!
//! **Custom schemes bypass the capability ACL** (they are registered for
//! every webview), so every request is authorised here (§6.4 rule 1):
//! anything not exactly right is `404` with an empty body and no reason.
//!
//! **Logs (I6)** carry a closed outcome kind, the status, a size bucket and
//! the elapsed time; never the token, the blob, the range or the URI.

pub mod cache;
pub mod engine;
pub mod exec;
pub mod helper;
pub mod range;
pub mod tokens;

use std::time::Instant;

use oma_archive::{MediaClass, Rendition};

use self::range::{RangePlan, RangeSpec};
use self::tokens::TokenBytes;
use super::session::ViewerSession;
use super::ViewerHost;

/// The scheme's name. The hyphen is valid (RFC 3986 §3.1). A headless
/// WebKitGTK 2.52.6 smoke test routed requests for it to this handler;
/// a desktop run and WebView2 are UNVERIFIED until spike 5s (the
/// fallback name is `omamedia`).
pub const SCHEME: &str = "oma-media";
/// The only webview that may use the scheme.
pub const MAIN_LABEL: &str = "main";
/// A `stream` chunk's largest size.
pub const CHUNK_BYTES: u64 = 2 << 20;
/// The largest file served whole to a `stream` request without `Range`.
pub const WHOLE_LIMIT: u64 = 8 << 20;
/// The headers every response carries.
pub const SECURITY_HEADERS: [(&str, &str); 3] = [
    ("X-Content-Type-Options", "nosniff"),
    ("Cache-Control", "no-store"),
    ("Content-Security-Policy", "default-src 'none'; sandbox"),
];

/// Where the frontend points `src`: `{mediaBase}v1/{token}/{rendition}`.
/// Linux serves the scheme itself; on Windows WebView2 serves custom
/// schemes as `http://<scheme>.localhost` (`use_https_scheme` stays
/// false).
pub const fn media_base() -> &'static str {
    if cfg!(windows) {
        "http://oma-media.localhost/"
    } else {
        "oma-media://localhost/"
    }
}

/// What the protocol answers.
#[derive(Clone, PartialEq, Eq)]
pub struct MediaResponse {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for MediaResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaResponse")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl MediaResponse {
    fn with_status(status: u16) -> MediaResponse {
        MediaResponse {
            status,
            headers: SECURITY_HEADERS
                .iter()
                .map(|(k, v)| (*k, (*v).to_owned()))
                .collect(),
            body: Vec::new(),
        }
    }

    fn with_body(status: u16, mime: &str, body: Vec<u8>) -> MediaResponse {
        let mut r = MediaResponse::with_status(status);
        r.headers.push(("Content-Type", mime.to_owned()));
        r.headers.push(("Content-Length", body.len().to_string()));
        r.body = body;
        r
    }

    /// `404`, empty, no reason: every refusal.
    pub fn refused() -> MediaResponse {
        MediaResponse::with_status(404)
    }

    /// `503`: dropped from a full queue, or the session ended.
    pub fn busy() -> MediaResponse {
        MediaResponse::with_status(503)
    }

    /// `200` with a rendition.
    pub fn image(mime: &str, body: Vec<u8>) -> MediaResponse {
        MediaResponse::with_body(200, mime, body)
    }

    /// `200` with a whole (small) stream.
    pub fn whole(mime: &str, body: Vec<u8>) -> MediaResponse {
        let mut r = MediaResponse::with_body(200, mime, body);
        r.headers.push(("Accept-Ranges", "bytes".to_owned()));
        r
    }

    /// `206` with bytes `start..=end` of `len`.
    pub fn partial(mime: &str, start: u64, end: u64, len: u64, body: Vec<u8>) -> MediaResponse {
        let mut r = MediaResponse::with_body(206, mime, body);
        r.headers
            .push(("Content-Range", format!("bytes {start}-{end}/{len}")));
        r.headers.push(("Accept-Ranges", "bytes".to_owned()));
        r
    }

    /// `416` with `Content-Range: bytes */len`.
    pub fn unsatisfiable(len: u64) -> MediaResponse {
        let mut r = MediaResponse::with_status(416);
        r.headers.push(("Content-Range", format!("bytes */{len}")));
        r
    }

    /// The value of header `name` (for tests).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// How a request ended, for the log: a closed kind, never a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Refused,
    Cached,
    Decoded,
    DecodeFailed,
    HelperCrashed,
    Streamed,
    Unsatisfiable,
    Unreadable,
    Dropped,
}

impl Outcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Outcome::Refused => "refused",
            Outcome::Cached => "cached",
            Outcome::Decoded => "decoded",
            Outcome::DecodeFailed => "decodeFailed",
            Outcome::HelperCrashed => "helperCrashed",
            Outcome::Streamed => "streamed",
            Outcome::Unsatisfiable => "unsatisfiable",
            Outcome::Unreadable => "unreadable",
            Outcome::Dropped => "dropped",
        }
    }
}

/// A body size as a coarse bucket (§6.4 rule 5): an exact size can
/// identify a known file.
pub fn size_bucket(n: usize) -> &'static str {
    match n {
        0..=0xFFFF => "<64KiB",
        0x1_0000..=0xF_FFFF => "<1MiB",
        0x10_0000..=0xFF_FFFF => "<16MiB",
        _ => ">=16MiB",
    }
}

/// The callback that hands a response to the webview.
pub type Respond = Box<dyn FnOnce(MediaResponse) + Send>;

/// One request's answer, sent exactly once: a reply dropped unanswered
/// (a closed queue, a worker that stopped) answers `503` itself, so no
/// request is left hanging.
pub struct Reply {
    respond: Option<Respond>,
    head: bool,
    started: Instant,
}

impl Reply {
    pub fn new(respond: Respond) -> Reply {
        Reply {
            respond: Some(respond),
            head: false,
            started: Instant::now(),
        }
    }

    /// Answer, logging the outcome kind, the status, the size bucket and
    /// the elapsed time. A `HEAD` request gets the headers only.
    pub fn send(mut self, outcome: Outcome, mut resp: MediaResponse) {
        let size = size_bucket(resp.body.len());
        if self.head {
            resp.body = Vec::new();
        }
        let status = resp.status;
        if let Some(f) = self.respond.take() {
            f(resp);
        }
        tracing::debug!(
            outcome = outcome.as_str(),
            status,
            size,
            elapsed_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "media"
        );
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        if let Some(f) = self.respond.take() {
            f(MediaResponse::busy());
            tracing::debug!(outcome = Outcome::Dropped.as_str(), status = 503, "media");
        }
    }
}

/// A request, as the Tauri adapter hands it over (no Tauri types).
#[derive(Debug, Clone, Copy)]
pub struct MediaRequest<'a> {
    /// The label of the webview that made the request.
    pub label: &'a str,
    pub method: &'a str,
    /// The URI's path and query, exactly as received.
    pub target: &'a str,
    /// Every `Range` header value (a non-UTF-8 value as `""`).
    pub ranges: &'a [&'a str],
    /// Every `Origin` header value.
    pub origins: &'a [&'a str],
    /// `mediaPlayback` is not `off`.
    pub playback_allowed: bool,
}

/// A parsed `/v1/<token>/<rendition>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub token: TokenBytes,
    pub rendition: Served,
}

/// The renditions the protocol knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    Thumb,
    Display,
    Stream,
}

/// Exactly `/v1/<32 lowercase hex>/<thumb|display|stream>`: no query, no
/// fragment, no escapes, nothing else.
pub fn parse_target(target: &str) -> Option<Target> {
    let rest = target.strip_prefix("/v1/")?;
    let (token, rendition) = rest.split_once('/')?;
    let rendition = match rendition {
        "thumb" => Served::Thumb,
        "display" => Served::Display,
        "stream" => Served::Stream,
        _ => return None,
    };
    Some(Target {
        token: TokenBytes::parse(token)?,
        rendition,
    })
}

/// What a `stream` request of a file of `len` bytes gets (§6.4 rule 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamPlan {
    /// No `Range`, and the file is at most [`WHOLE_LIMIT`]: `200`, whole.
    Whole,
    /// `206` with bytes `start..=end` (at most [`CHUNK_BYTES`]).
    Partial { start: u64, end: u64 },
    /// `416` with `bytes */len`.
    Unsatisfiable,
    /// No `Range` and over [`WHOLE_LIMIT`]: refused (`206` is never sent
    /// to a request without `Range`, RFC 9110 §15.3.7).
    Refused,
}

pub fn stream_plan(range: Option<RangeSpec>, len: u64) -> StreamPlan {
    match range {
        None if len > WHOLE_LIMIT => StreamPlan::Refused,
        None => StreamPlan::Whole,
        Some(spec) => match range::plan(spec, len, CHUNK_BYTES) {
            RangePlan::Unsatisfiable => StreamPlan::Unsatisfiable,
            RangePlan::Partial { start, end } => StreamPlan::Partial { start, end },
        },
    }
}

/// Handle one request. Returns at once: the answer is sent now (a
/// refusal, a cache hit, a `416`) or by a worker.
pub fn dispatch(host: &ViewerHost, req: &MediaRequest<'_>, mut reply: Reply) {
    let refuse = |reply: Reply| reply.send(Outcome::Refused, MediaResponse::refused());
    if req.label != MAIN_LABEL {
        return refuse(reply);
    }
    match req.method {
        "GET" => {}
        "HEAD" => reply.head = true,
        _ => return refuse(reply),
    }
    let Some(target) = parse_target(req.target) else {
        return refuse(reply);
    };
    match req.origins {
        [] => {}
        [one] if host.origin_allowed(one) => {}
        _ => return refuse(reply),
    }
    let range = match req.ranges {
        [] => None,
        [one] => match range::parse(one) {
            Some(r) => Some(r),
            None => return refuse(reply),
        },
        _ => return refuse(reply),
    };
    let Some(session) = host.current() else {
        return refuse(reply);
    };
    serve(&session, target, range, req.playback_allowed, reply);
}

fn serve(
    s: &ViewerSession,
    target: Target,
    range: Option<RangeSpec>,
    playback_allowed: bool,
    reply: Reply,
) {
    let refuse = |reply: Reply| reply.send(Outcome::Refused, MediaResponse::refused());
    let media = s.media();
    let Some(entry) = media.lookup(target.token) else {
        return refuse(reply);
    };
    let Some(found) = entry.inspected else {
        return refuse(reply);
    };
    let class = found.kind.class();
    let rendition = match target.rendition {
        Served::Thumb => Rendition::Thumb,
        Served::Display => Rendition::Display,
        Served::Stream => {
            if !matches!(class, MediaClass::Playable { .. }) || !playback_allowed {
                return refuse(reply);
            }
            let Some(ident) = s.stream_identity(found.blob) else {
                return refuse(reply);
            };
            let len = ident.len();
            let (start, end, whole) = match stream_plan(range, len) {
                StreamPlan::Refused => return refuse(reply),
                StreamPlan::Whole => (0, len.saturating_sub(1), true),
                StreamPlan::Unsatisfiable => {
                    return reply.send(Outcome::Unsatisfiable, MediaResponse::unsatisfiable(len))
                }
                StreamPlan::Partial { start, end } => (start, end, false),
            };
            return media.request_stream(engine::StreamJob {
                blob: found.blob,
                ident,
                mime: found.kind.mime(),
                start,
                end,
                whole,
                reply,
            });
        }
    };
    if class != MediaClass::Image {
        return refuse(reply);
    }
    media.request_rendition(found.blob, rendition, reply);
}

#[cfg(test)]
mod tests;
