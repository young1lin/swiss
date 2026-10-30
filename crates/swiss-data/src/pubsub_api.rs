/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The redis pub/sub routes of /api/db (SPEC §data.pubsub): the channels that have a subscriber
//! right now, and a live feed of what is published on a watch set, streamed as NDJSON for as
//! long as the reader keeps it open. Publishing needs no route of its own - PUBLISH is an
//! ordinary command, and the panel sends it through /redis-pipeline under the console's guard.
//!
//! The house rules of `dbbrowser_api` hold: a bad watch set is a 400 before any lease is taken,
//! and the lease lives as long as the request - for a feed, as long as its body, which gives it
//! back the moment a stopping MCP plugin recalls it rather than when the drain gives up.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::{Path, Query};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::{json, Map, Value};

use swiss_host::dbbrowser::{pubsub_watch, PubSubFeed};
use swiss_host::services::catalog::{CatalogRegistry, ConnectionLease};

use crate::dbbrowser_api::{lease_redis, reply, Fail};

/// The last event of a feed that a stopping MCP plugin recalled.
const RECALLED: &str = "the mcp plugin is stopping";

/// GET /pubsub?pattern=: the channels with a subscriber, their counts and the pattern total.
async fn channels(
    catalog: &CatalogRegistry,
    name: &str,
    q: &HashMap<String, String>,
) -> Result<Value, Fail> {
    let (_lease, rb) = lease_redis(catalog, name)?;
    let mut o = Map::new();
    if let Some(pattern) = q.get("pattern").filter(|p| !p.trim().is_empty()) {
        o.insert("pattern".into(), json!(pattern));
    }
    rb.pubsub_channels(&Value::Object(o)).await.map_err(Fail::bad)
}

/// POST /pubsub/subscribe { channels?, patterns? }: the feed as NDJSON, one event a line. A
/// failure before the first event - the watch set, the lease, the connect, a refused SUBSCRIBE
/// - is the usual JSON error; once the 200 is out, the feed's end is its `closed` line.
async fn subscribe(catalog: &CatalogRegistry, name: &str, body: &Value) -> Result<Response, Fail> {
    let watch = pubsub_watch(body).map_err(Fail::bad)?;
    let (lease, rb) = lease_redis(catalog, name)?;
    let feed = rb.subscribe(&watch).await.map_err(Fail::bad)?;
    let body = FeedBody {
        feed,
        recalled: Box::pin(lease.recalled()),
        lease: Some(lease),
        done: false,
    };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        axum::body::Body::from_stream(body),
    )
        .into_response())
}

/// A feed as a response body: each event one line, the connection lease riding inside (SPEC
/// §host.seats). It ends when the feed does, or - with a `closed` line of its own - when the
/// lease is recalled; dropping it (the reader went away) closes the feed and its connection.
struct FeedBody {
    feed: PubSubFeed,
    lease: Option<ConnectionLease>,
    recalled: Pin<Box<dyn Future<Output = ()> + Send>>,
    done: bool,
}

impl FeedBody {
    fn finish(&mut self) {
        self.done = true;
        self.lease = None;
        self.feed.close();
    }
}

fn ndjson_line(event: &Value) -> axum::body::Bytes {
    let mut line = event.to_string();
    line.push('\n');
    axum::body::Bytes::from(line)
}

impl futures_core::Stream for FeedBody {
    type Item = Result<axum::body::Bytes, axum::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        if this.recalled.as_mut().poll(cx).is_ready() {
            this.finish();
            let closed = json!({ "event": "closed", "reason": RECALLED });
            return Poll::Ready(Some(Ok(ndjson_line(&closed))));
        }
        match this.feed.poll_recv(cx) {
            Poll::Ready(Some(event)) => Poll::Ready(Some(Ok(ndjson_line(&event)))),
            // The producer is done (its closed line went out before this): the lease may go.
            Poll::Ready(None) => {
                this.finish();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

// --- the axum handlers -----------------------------------------------------------------------------

type Catalog = Extension<Arc<CatalogRegistry>>;

async fn channels_route(
    Extension(catalog): Catalog,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    reply(channels(&catalog, &name, &q).await)
}

async fn subscribe_route(
    Extension(catalog): Catalog,
    Path(name): Path<String>,
    body: swiss_host::reply::NodeBody,
) -> Response {
    match subscribe(&catalog, &name, &body.0).await {
        Ok(response) => response,
        Err(f) => swiss_host::reply::admin_error(f.status, &f.message),
    }
}

/// The pub/sub routes, merged into `dbbrowser_router` - which layers the catalog over them.
pub(crate) fn pubsub_router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/db/{name}/pubsub", get(channels_route))
        .route("/api/db/{name}/pubsub/subscribe", post(subscribe_route))
}

#[cfg(test)]
mod tests {
    //! The pub/sub routes end to end over a stub redis browser: the 404 for another flavour, the
    //! 400 before the lease, the NDJSON body, and a feed's lease - held while it runs, given back
    //! when it ends or is recalled.
    use super::*;
    use async_trait::async_trait;
    use axum::body::Body as HttpBody;
    use axum::http::Request;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use swiss_host::dbbrowser::{BrowserFlavor, PubSubWatch, RedisBrowser};
    use swiss_host::services::catalog::{
        drain_leases, CatalogError, ConnectionCatalog, ConnectionInfo, LeaseTracker,
    };
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    #[derive(Default)]
    struct Seen {
        channels: Vec<Value>,
        watches: Vec<PubSubWatch>,
        /// The producer side of the last feed, so a test decides when it ends.
        feed: Option<mpsc::Sender<Value>>,
    }

    struct StubRedis {
        seen: Arc<Mutex<Seen>>,
    }

    #[async_trait]
    impl RedisBrowser for StubRedis {
        fn label(&self) -> String {
            "cache @ localhost:6379 db 0".into()
        }
        async fn list_keys(&self, _o: &Value) -> Result<Value, String> {
            Ok(json!({ "keys": [], "cursor": "0", "done": true }))
        }
        async fn read_key(&self, key: &str) -> Result<Value, String> {
            Ok(json!({ "key": key }))
        }
        async fn run_command(&self, _line: &str) -> Result<Value, String> {
            Ok(json!("OK"))
        }
        async fn run_pipeline(&self, _commands: &[Vec<String>]) -> Result<Value, String> {
            Ok(json!({ "replies": [] }))
        }
        async fn pubsub_channels(&self, o: &Value) -> Result<Value, String> {
            self.seen.lock().unwrap().channels.push(o.clone());
            Ok(json!({ "pattern": "*", "channels": [{ "name": "orders", "subscribers": 2 }], "total": 1, "truncated": false, "patterns": 0 }))
        }
        async fn subscribe(&self, watch: &PubSubWatch) -> Result<PubSubFeed, String> {
            if watch.channels.iter().any(|c| c == "refused") {
                return Err("ERR SUBSCRIBE is disabled here".into());
            }
            let (tx, rx) = mpsc::channel(8);
            tx.try_send(json!({ "event": "subscribed", "channels": watch.channels, "patterns": watch.patterns })).unwrap();
            tx.try_send(json!({ "event": "message", "channel": "orders", "payload": "{\"id\":1}", "size": 8, "at": 1 })).unwrap();
            let mut seen = self.seen.lock().unwrap();
            seen.watches.push(watch.clone());
            seen.feed = Some(tx);
            Ok(rx)
        }
    }

    struct Provider {
        redis: Arc<dyn RedisBrowser>,
        tracker: Arc<LeaseTracker>,
    }

    impl ConnectionCatalog for Provider {
        fn list(&self) -> Vec<ConnectionInfo> {
            ["r", "x"]
                .into_iter()
                .map(|id| ConnectionInfo { id: id.into(), label: id.into(), dialect: "redis".into(), state: "running".into() })
                .collect()
        }
        fn lease(&self, id: &str, holder: &str) -> Result<ConnectionLease, CatalogError> {
            match id {
                "r" => Ok(self.tracker.grant(id, holder, "redis", BrowserFlavor::Redis(self.redis.clone()))),
                "x" => Ok(self.tracker.grant(id, holder, "echo", BrowserFlavor::None)),
                _ => Err(CatalogError::Unknown(format!("unknown MCP: {id}"))),
            }
        }
    }

    fn app() -> (Router<()>, Arc<Mutex<Seen>>, Arc<LeaseTracker>) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let tracker = LeaseTracker::new();
        let catalog = Arc::new(CatalogRegistry::new());
        catalog
            .register(
                Arc::new(Provider { redis: Arc::new(StubRedis { seen: seen.clone() }), tracker: tracker.clone() }),
                "mcp",
            )
            .expect("test provider registers");
        let router = crate::dbbrowser_api::dbbrowser_router(catalog, Arc::new(Vec::new), Arc::new(|_| "default".to_string()));
        (router, seen, tracker)
    }

    async fn send(router: Router<()>, method: &str, uri: &str, body: Option<Value>) -> Response {
        let builder = Request::builder().method(method).uri(uri);
        let req = match body {
            Some(v) => builder.header("content-type", "application/json").body(HttpBody::from(v.to_string())).expect("request"),
            None => builder.body(HttpBody::empty()).expect("request"),
        };
        router.oneshot(req).await.expect("infallible router")
    }

    async fn json_of(resp: Response) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("body");
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// The body's lines one at a time - None once it ends.
    struct Lines(axum::body::BodyDataStream);

    impl Lines {
        async fn next(&mut self) -> Option<Value> {
            let chunk = std::future::poll_fn(|cx| futures_core::Stream::poll_next(Pin::new(&mut self.0), cx));
            let bytes = tokio::time::timeout(Duration::from_secs(5), chunk).await.expect("a line within 5s")?.expect("chunk");
            let text = std::str::from_utf8(&bytes).expect("utf-8");
            assert!(text.ends_with('\n') && text.matches('\n').count() == 1, "one event a line: {text:?}");
            Some(serde_json::from_str(text).expect("json line"))
        }
    }

    #[tokio::test]
    async fn the_pubsub_routes_answer_only_a_redis_connection() {
        let (router, seen, _) = app();
        let (status, body) = json_of(send(router.clone(), "GET", "/api/db/x/pubsub", None).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].as_str().unwrap().contains("is not a redis connection"), "{body}");
        let resp = send(router, "POST", "/api/db/x/pubsub/subscribe", Some(json!({ "channels": ["a"] }))).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(seen.lock().unwrap().watches.is_empty());
    }

    #[tokio::test]
    async fn the_channel_listing_passes_its_pattern_through() {
        let (router, seen, tracker) = app();
        let (status, body) = json_of(send(router.clone(), "GET", "/api/db/r/pubsub?pattern=ord*", None).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["channels"][0]["subscribers"], json!(2));
        send(router.clone(), "GET", "/api/db/r/pubsub?pattern=%20", None).await;
        send(router, "GET", "/api/db/r/pubsub", None).await;
        assert_eq!(seen.lock().unwrap().channels, [json!({ "pattern": "ord*" }), json!({}), json!({})]);
        assert_eq!(tracker.outstanding(), 0);
    }

    #[tokio::test]
    async fn a_bad_watch_set_is_a_400_before_any_lease() {
        let (router, seen, tracker) = app();
        for (body, says) in [
            (json!({}), "name at least one channel or pattern"),
            (json!({ "channels": "orders" }), "channels must be an array"),
            (json!({ "patterns": [""] }), "patterns holds an empty name"),
        ] {
            let (status, out) = json_of(send(router.clone(), "POST", "/api/db/x/pubsub/subscribe", Some(body)).await).await;
            // Checked before the lease: the connection's flavour is never even asked.
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(out["error"].as_str().unwrap().contains(says), "{out}");
        }
        let (status, out) = json_of(send(router, "POST", "/api/db/r/pubsub/subscribe", Some(json!({ "channels": ["refused"] }))).await).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(out["error"].as_str().unwrap().contains("SUBSCRIBE is disabled here"), "{out}");
        assert!(seen.lock().unwrap().watches.is_empty());
        assert_eq!(tracker.outstanding(), 0);
    }

    #[tokio::test]
    async fn a_feed_streams_ndjson_and_holds_its_lease_until_it_ends() {
        let (router, seen, tracker) = app();
        let resp = send(router, "POST", "/api/db/r/pubsub/subscribe", Some(json!({ "channels": ["orders"], "patterns": ["order*"] }))).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers()[header::CONTENT_TYPE].to_str().unwrap().starts_with("application/x-ndjson"));
        let mut lines = Lines(resp.into_body().into_data_stream());
        assert_eq!(lines.next().await, Some(json!({ "event": "subscribed", "channels": ["orders"], "patterns": ["order*"] })));
        assert_eq!(lines.next().await.unwrap()["payload"], json!("{\"id\":1}"));
        assert_eq!(tracker.outstanding(), 1, "a live feed holds its lease");
        let tx = seen.lock().unwrap().feed.take().unwrap();
        tx.send(json!({ "event": "closed", "reason": "the server closed the connection" })).await.unwrap();
        drop(tx);
        assert_eq!(lines.next().await.unwrap()["event"], json!("closed"));
        assert_eq!(lines.next().await, None);
        assert_eq!(tracker.outstanding(), 0);
    }

    #[tokio::test]
    async fn a_stopping_plugin_recalls_a_live_feed_instead_of_waiting_it_out() {
        let (router, seen, tracker) = app();
        let resp = send(router, "POST", "/api/db/r/pubsub/subscribe", Some(json!({ "channels": ["orders"] }))).await;
        let mut lines = Lines(resp.into_body().into_data_stream());
        lines.next().await.unwrap();
        lines.next().await.unwrap();
        let reader = tokio::spawn(async move {
            let mut rest = Vec::new();
            while let Some(line) = lines.next().await {
                rest.push(line);
            }
            rest
        });
        let started = Instant::now();
        assert_eq!(drain_leases(&tracker, Duration::from_secs(5), "test").await, 0);
        assert!(started.elapsed() < Duration::from_secs(1), "drained in {:?}", started.elapsed());
        assert_eq!(reader.await.unwrap(), [json!({ "event": "closed", "reason": RECALLED })]);
        // The producer hears it too, and stops.
        assert!(seen.lock().unwrap().feed.as_ref().unwrap().is_closed());
    }
}
