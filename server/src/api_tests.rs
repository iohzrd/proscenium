//! Tests of the HTTP routes through the router built by `api::build_router`, with
//! in-memory storage and a node bound to 127.0.0.1 without relay or DNS.

use crate::api::{AppState, build_router};
use crate::config::Config;
use crate::http::RateLimiter;
use crate::ingestion::IngestionManager;
use crate::node::Node;
use crate::storage::Storage;
use crate::test_util::{TestUser, loopback_endpoint};
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use iroh::SecretKey;
use proscenium_types::{REGISTRATION_ACTION_UNREGISTER, Visibility, now_millis};
use serde::Serialize;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tower::{Layer, Service};

const URL: &str = "https://server.test";
const REGISTER: &str = "/api/v1/register";

struct TestApp {
    router: Router,
    storage: Arc<Storage>,
    manager: Arc<IngestionManager>,
    node: Node,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

impl TestApp {
    async fn new(extra_config: &str) -> Self {
        let config = Config::parse(&format!(
            "[server]\npublic_url = \"{URL}\"\n[sync]\nstartup_sync = false\n{extra_config}"
        ))
        .unwrap();
        let storage = Arc::new(Storage::open_in_memory().await.unwrap());
        let node = Node::spawn(loopback_endpoint(SecretKey::from_bytes(&[200u8; 32])).await);
        let manager = IngestionManager::new(&node, storage.clone(), &config);
        let register_limiter = Arc::new(RateLimiter::new(
            config.limits.register_requests_per_minute,
            Duration::from_secs(60),
        ));
        let router = build_router(AppState {
            storage: storage.clone(),
            config: Arc::new(config),
            ingestion: manager.clone(),
            start_time: std::time::Instant::now(),
            register_limiter,
        });
        Self {
            router,
            storage,
            manager,
            node,
        }
    }

    async fn send(&self, request: Request<Body>) -> Reply {
        // `http::serve` adds the peer address the same way.
        let peer: SocketAddr = "192.0.2.10:5000".parse().unwrap();
        let mut service = axum::Extension(ConnectInfo(peer)).layer(self.router.clone());
        let response = service.call(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap();
        Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    async fn get(&self, path: &str) -> Reply {
        self.send(Request::get(path).body(Body::empty()).unwrap())
            .await
    }

    async fn send_json(&self, method: Method, path: &str, body: &impl Serialize) -> Reply {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap();
        self.send(request).await
    }

    async fn shutdown(self) {
        self.manager.shutdown().await;
        self.node.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_lifecycle_over_http() {
    let app = TestApp::new("[limits]\nregister_requests_per_minute = 1000\n").await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    let now = now_millis();

    let request = user.registration(URL, now, Visibility::Public, None);
    let reply = app.send_json(Method::POST, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.json()["pubkey"], pk.as_str());
    assert!(app.manager.has_subscription(pk).await);

    // The same body again is a replay.
    let reply = app.send_json(Method::POST, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);

    // A newer request updates the registration.
    let request = user.registration(URL, now + 1, Visibility::Public, None);
    let reply = app.send_json(Method::POST, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);

    // A forged delegation: the attacker names the victim as master.
    let attacker = TestUser::new(2);
    let mut forged = attacker.registration(URL, now + 2, Visibility::Public, None);
    forged.master_pubkey = pk.clone();
    forged.delegation.master_pubkey = pk.clone();
    forged.signature = attacker.sign_registration_request(&forged);
    let reply = app.send_json(Method::POST, REGISTER, &forged).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    let unregister = Some(REGISTRATION_ACTION_UNREGISTER);
    forged.action = unregister.map(str::to_string);
    forged.signature = attacker.sign_registration_request(&forged);
    let reply = app.send_json(Method::DELETE, REGISTER, &forged).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);

    // A rotated-out signing key with its master-signed delegation.
    let rotated = user.rotated(1);
    let request = rotated.registration(URL, now + 3, Visibility::Public, None);
    let reply = app.send_json(Method::POST, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let request = user.registration(URL, now + 4, Visibility::Public, None);
    let reply = app.send_json(Method::POST, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
    let request = user.registration(URL, now + 4, Visibility::Public, unregister);
    let reply = app.send_json(Method::DELETE, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);

    // Profile update with the current key.
    let profile = rotated.profile("Alice", Visibility::Public, now);
    let update = rotated.profile_update(URL, now + 5, profile.clone());
    let reply = app.send_json(Method::PUT, REGISTER, &update).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let reply = app.get(&format!("/api/v1/users/{pk}")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["display_name"], "Alice");
    assert_eq!(reply.json()["visibility"], "public");

    // The same update again is a replay; one with a changed field fails verification.
    let reply = app.send_json(Method::PUT, REGISTER, &update).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
    let mut altered = rotated.profile_update(URL, now + 6, profile);
    altered.profile.display_name = "Mallory".to_string();
    let reply = app.send_json(Method::PUT, REGISTER, &altered).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);

    // Device NodeIds of the registered user.
    let reply = app.get(&format!("/api/v1/users/{pk}/devices")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.json()["transport_node_ids"],
        serde_json::json!([user.transport_node_id])
    );

    // Private visibility: rejected on POST; on PUT the registration is removed.
    let request = rotated.registration(URL, now + 7, Visibility::Private, None);
    let reply = app.send_json(Method::POST, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
    assert_eq!(app.storage.registration_count().await.unwrap(), 1);

    // DELETE without the unregister action, then a valid DELETE.
    let request = rotated.registration(URL, now + 8, Visibility::Public, None);
    let reply = app.send_json(Method::DELETE, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    let request = rotated.registration(URL, now + 8, Visibility::Public, unregister);
    let reply = app.send_json(Method::DELETE, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(!app.manager.has_subscription(pk).await);

    for path in [
        format!("/api/v1/users/{pk}"),
        format!("/api/v1/users/{pk}/devices"),
        format!("/user/{pk}"),
    ] {
        assert_eq!(app.get(&path).await.status, StatusCode::NOT_FOUND, "{path}");
    }
    // The same DELETE again is a replay. A new DELETE for the inactive registration
    // succeeds without a change; a profile update for it is a 404.
    let reply = app.send_json(Method::DELETE, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
    let request = rotated.registration(URL, now + 9, Visibility::Public, unregister);
    let reply = app.send_json(Method::DELETE, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.json()["message"], "not registered");
    let profile = rotated.profile("Alice", Visibility::Public, now);
    let update = rotated.profile_update(URL, now + 10, profile);
    let reply = app.send_json(Method::PUT, REGISTER, &update).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);

    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_profile_update_removes_registration() {
    let app = TestApp::new("").await;
    let user = TestUser::new(1);
    let now = now_millis();
    let request = user.registration(URL, now, Visibility::Public, None);
    let reply = app.send_json(Method::POST, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);

    let profile = user.profile("Alice", Visibility::Private, now);
    let update = user.profile_update(URL, now + 1, profile);
    let reply = app.send_json(Method::PUT, REGISTER, &update).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
    assert_eq!(app.storage.registration_count().await.unwrap(), 0);
    assert!(!app.manager.has_subscription(&user.master_pubkey).await);

    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_limits_over_http() {
    let app =
        TestApp::new("[limits]\nmax_registered_users = 1\nregister_requests_per_minute = 4\n")
            .await;
    let now = now_millis();
    let first = TestUser::new(1);
    let second = TestUser::new(2);

    let request = first.registration(URL, now, Visibility::Listed, None);
    let reply = app.send_json(Method::POST, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    // A listed registration has no gossip task.
    assert!(!app.manager.has_subscription(&first.master_pubkey).await);

    let request = second.registration(URL, now, Visibility::Public, None);
    let reply = app.send_json(Method::POST, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);

    // A body above the size limit, and a body that is not a registration request.
    let request = Request::post(REGISTER)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(vec![b' '; 64 * 1024]))
        .unwrap();
    assert_eq!(
        app.send(request).await.status,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let reply = app
        .send_json(Method::POST, REGISTER, &serde_json::json!({"a": 1}))
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);

    // The fifth request of one address within the window is refused before the handler.
    let request = second.registration(URL, now + 1, Visibility::Public, None);
    let reply = app.send_json(Method::POST, REGISTER, &request).await;
    assert_eq!(
        reply.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        reply.body
    );
    assert_eq!(reply.headers.get(header::RETRY_AFTER).unwrap(), "60");
    // Read routes are not counted.
    assert_eq!(app.get("/api/v1/info").await.status, StatusCode::OK);

    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_routes_validate_input_and_filter_by_visibility() {
    let app = TestApp::new("").await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    let now = now_millis();
    let request = user.registration(URL, now, Visibility::Public, None);
    let reply = app.send_json(Method::POST, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);

    let post = user.post(
        "hello #rust <b>world</b> https://example.com/a#b",
        now - 1_000,
    );
    let other = user.post("second post", now - 2_000);
    app.storage
        .insert_posts(pk, &[post.clone(), other], 100)
        .await
        .unwrap();
    app.storage
        .insert_interactions(pk, &[user.like(&post, 1, now)], 100)
        .await
        .unwrap();

    // `limit` is clamped to at least 1 and at most the route maximum.
    let reply = app.get("/api/v1/feed?limit=-1").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["posts"].as_array().unwrap().len(), 1);
    let reply = app.get("/api/v1/feed?limit=1000000").await;
    assert_eq!(reply.json()["posts"].as_array().unwrap().len(), 2);
    let reply = app.get("/api/v1/users?limit=-5&offset=-5").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["limit"], 1);
    assert_eq!(reply.json()["offset"], 0);
    assert_eq!(reply.json()["total"], 1);
    let reply = app.get(&format!("/api/v1/users/{pk}/posts?limit=-1")).await;
    assert_eq!(reply.json()["posts"].as_array().unwrap().len(), 1);
    let reply = app.get("/api/v1/trending?limit=-1").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(app.get("/users?offset=-10").await.status, StatusCode::OK);
    assert_eq!(
        app.get("/api/v1/feed?authors=x").await.status,
        StatusCode::BAD_REQUEST
    );
    let reply = app.get(&format!("/api/v1/feed?authors={pk}")).await;
    assert_eq!(reply.json()["posts"].as_array().unwrap().len(), 2);

    // Search: input with FTS5 syntax characters is a phrase query; input without a
    // term is rejected with 400.
    for (query, expected) in [("rust", 1), ("%23rust", 1), ("c%2B%2B", 0), ("%22hello", 1)] {
        let reply = app.get(&format!("/api/v1/posts/search?q={query}")).await;
        assert_eq!(reply.status, StatusCode::OK, "{query}");
        assert_eq!(reply.json()["total"], expected, "{query}");
    }
    for query in ["", "%23", "%2B%2B", "%22%22"] {
        let reply = app.get(&format!("/api/v1/posts/search?q={query}")).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{query:?}");
    }
    let reply = app
        .get("/api/v1/posts/search?q=rust&limit=-1&offset=-1")
        .await;
    assert_eq!(reply.status, StatusCode::OK);

    // Interactions of a stored post; 404 for a post that is not stored.
    let reply = app
        .get(&format!("/api/v1/posts/{pk}/{}/interactions", post.id))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["like_count"], 1);
    let missing = "0".repeat(32);
    let reply = app
        .get(&format!("/api/v1/posts/{pk}/{missing}/interactions"))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // HTML pages. The hashtag page returns the post; an invalid tag is a 400.
    let page = app.get("/trending?tag=rust").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("&lt;b&gt;world&lt;/b&gt;"),
        "{}",
        page.body
    );
    assert!(
        page.body
            .contains(r#"<a href="/trending?tag=rust">#rust</a>"#),
        "{}",
        page.body
    );
    assert!(
        page.body.contains(
            r#"<a href="https://example.com/a#b" target="_blank" rel="noopener noreferrer">"#
        ),
        "{}",
        page.body
    );
    // The word occurs in no hashtag of the other post.
    assert!(!page.body.contains("second post"));
    let page = app.get("/trending?tag=second").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("No posts found with this tag."));
    assert_eq!(
        app.get("/trending?tag=a%20b").await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.get("/trending?tag=%22").await.status,
        StatusCode::BAD_REQUEST
    );
    for path in [
        "/".to_string(),
        "/trending".to_string(),
        "/users".to_string(),
        format!("/user/{pk}"),
        format!("/post/{pk}/{}", post.id),
    ] {
        assert_eq!(app.get(&path).await.status, StatusCode::OK, "{path}");
    }
    assert_eq!(
        app.get(&format!("/post/{pk}/{missing}")).await.status,
        StatusCode::NOT_FOUND
    );

    // After the user becomes listed, no route returns the posts.
    let request = user.registration(URL, now + 1, Visibility::Listed, None);
    let reply = app.send_json(Method::POST, REGISTER, &request).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(!app.manager.has_subscription(pk).await);
    let reply = app.get("/api/v1/feed").await;
    assert!(reply.json()["posts"].as_array().unwrap().is_empty());
    let reply = app.get(&format!("/api/v1/users/{pk}/posts")).await;
    assert!(reply.json()["posts"].as_array().unwrap().is_empty());
    assert_eq!(
        app.get(&format!("/post/{pk}/{}", post.id)).await.status,
        StatusCode::NOT_FOUND
    );
    let reply = app
        .get(&format!("/api/v1/posts/{pk}/{}/interactions", post.id))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let page = app.get(&format!("/user/{pk}")).await;
    assert!(page.body.contains("not publicly visible"));
    // The directory still lists the user and its device NodeId.
    let reply = app.get(&format!("/api/v1/users/{pk}/devices")).await;
    assert_eq!(reply.status, StatusCode::OK);

    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_headers() {
    let app = TestApp::new("").await;

    // HTML pages carry the content security policy and frame headers.
    let page = app.get("/").await;
    assert_eq!(page.status, StatusCode::OK);
    let csp = page.headers.get(header::CONTENT_SECURITY_POLICY).unwrap();
    assert!(csp.to_str().unwrap().starts_with("default-src 'none'"));
    assert_eq!(page.headers.get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
    assert_eq!(
        page.headers.get(header::REFERRER_POLICY).unwrap(),
        "no-referrer"
    );
    assert_eq!(
        page.headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
        "nosniff"
    );

    // Read routes allow any origin.
    let request = Request::get("/api/v1/info")
        .header(header::ORIGIN, "https://other.test")
        .body(Body::empty())
        .unwrap();
    let reply = app.send(request).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply
            .headers
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .unwrap(),
        "*"
    );
    assert_eq!(
        reply.headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
        "nosniff"
    );
    assert!(reply.headers.get(header::CONTENT_SECURITY_POLICY).is_none());

    // The registration route answers a preflight without any CORS header, so a browser
    // does not send the cross-origin request.
    let request = Request::builder()
        .method(Method::OPTIONS)
        .uri(REGISTER)
        .header(header::ORIGIN, "https://other.test")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
        .body(Body::empty())
        .unwrap();
    let reply = app.send(request).await;
    assert!(
        reply
            .headers
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
    assert!(
        reply
            .headers
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .is_none()
    );

    app.shutdown().await;
}
