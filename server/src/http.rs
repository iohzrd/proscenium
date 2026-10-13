//! HTTP/1 accept loop with a connection limit, a header read timeout, an I/O idle
//! timeout and graceful shutdown; and the per-address request counter used for
//! `/api/v1/register`.

use axum::Router;
use axum::extract::ConnectInfo;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tower::Layer;

const ACCEPT_ERROR_DELAY: Duration = Duration::from_secs(1);

/// Connection limits of `serve`.
#[derive(Debug, Clone, Copy)]
pub struct ServeLimits {
    /// Maximum number of open connections. Further connections wait in the listen
    /// backlog.
    pub max_connections: usize,
    /// Time a client has to send a complete request header. Also closes a keep-alive
    /// connection on which no next request starts within this time.
    pub header_read_timeout: Duration,
    /// A connection on which no byte was read or written for this long is closed.
    /// Must be greater than `api::REQUEST_TIMEOUT`, during which a handler may run
    /// without I/O.
    pub io_idle_timeout: Duration,
    /// Time open connections get to finish after the shutdown signal.
    pub shutdown_grace: Duration,
}

impl ServeLimits {
    pub fn new(max_connections: usize) -> Self {
        Self {
            max_connections,
            header_read_timeout: Duration::from_secs(10),
            io_idle_timeout: Duration::from_secs(60),
            shutdown_grace: Duration::from_secs(10),
        }
    }
}

/// Stream wrapper that records the time of the last successful read or write.
struct ActivityIo<T> {
    inner: T,
    started: Instant,
    /// Milliseconds between `started` and the last read or write of at least one byte.
    last_active_ms: Arc<AtomicU64>,
}

impl<T> ActivityIo<T> {
    fn touch(&self) {
        let elapsed = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_active_ms.store(elapsed, Ordering::Relaxed);
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for ActivityIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.touch();
        }
        result
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for ActivityIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            self.touch();
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Serve `app` over HTTP/1 on `listener` until `shutdown` completes, then wait up to
/// `limits.shutdown_grace` for open connections.
///
/// The peer address of each connection is available to handlers as
/// `ConnectInfo<SocketAddr>`.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    limits: ServeLimits,
    shutdown: impl Future<Output = ()>,
) {
    let mut builder = Builder::new(TokioExecutor::new()).http1_only();
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_read_timeout);

    let idle_check_interval = (limits.io_idle_timeout / 6).max(Duration::from_millis(10));
    let slots = Arc::new(Semaphore::new(limits.max_connections));
    let graceful = GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown);

    loop {
        let permit = tokio::select! {
            permit = slots.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                // The semaphore is never closed.
                Err(_) => break,
            },
            _ = &mut shutdown => break,
        };
        let (stream, remote_addr) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(e) => {
                    if !is_connection_error(&e) {
                        // For example EMFILE: wait for other connections to close.
                        tracing::error!("[http] accept error: {e}");
                        tokio::time::sleep(ACCEPT_ERROR_DELAY).await;
                    }
                    continue;
                }
            },
            _ = &mut shutdown => break,
        };

        let started = Instant::now();
        let last_active_ms = Arc::new(AtomicU64::new(0));
        let io = TokioIo::new(ActivityIo {
            inner: stream,
            started,
            last_active_ms: last_active_ms.clone(),
        });
        let service =
            TowerToHyperService::new(axum::Extension(ConnectInfo(remote_addr)).layer(app.clone()));
        let connection = graceful.watch(builder.serve_connection(io, service).into_owned());

        tokio::spawn(async move {
            let _permit = permit;
            let mut connection = std::pin::pin!(connection);
            let mut check = tokio::time::interval(idle_check_interval);
            loop {
                tokio::select! {
                    result = connection.as_mut() => {
                        if let Err(e) = result {
                            tracing::debug!("[http] connection from {remote_addr} ended: {e}");
                        }
                        break;
                    }
                    _ = check.tick() => {
                        let last = Duration::from_millis(last_active_ms.load(Ordering::Relaxed));
                        if started.elapsed().saturating_sub(last) > limits.io_idle_timeout {
                            tracing::debug!("[http] closing idle connection from {remote_addr}");
                            break;
                        }
                    }
                }
            }
        });
    }

    drop(listener);
    if tokio::time::timeout(limits.shutdown_grace, graceful.shutdown())
        .await
        .is_err()
    {
        tracing::warn!(
            "[http] connections still open {}s after shutdown signal",
            limits.shutdown_grace.as_secs()
        );
    }
}

fn is_connection_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
    )
}

/// The address a request is counted under: the TCP peer address, or the last
/// `X-Forwarded-For` entry when `trust_forwarded_for` is set and the entry parses.
pub fn client_ip(
    headers: &axum::http::HeaderMap,
    remote_addr: SocketAddr,
    trust_forwarded_for: bool,
) -> IpAddr {
    if trust_forwarded_for
        && let Some(value) = headers.get_all("x-forwarded-for").iter().next_back()
        && let Ok(value) = value.to_str()
        && let Some(last) = value.rsplit(',').next()
        && let Ok(ip) = last.trim().parse::<IpAddr>()
    {
        return ip;
    }
    remote_addr.ip()
}

/// Maximum number of addresses tracked by one `RateLimiter`.
const MAX_TRACKED_ADDRESSES: usize = 100_000;

struct Window {
    started: Instant,
    count: u32,
}

/// Fixed-window request counter per client address. An IPv6 address is counted under
/// its /64 prefix.
pub struct RateLimiter {
    max_per_window: u32,
    window: Duration,
    windows: Mutex<HashMap<IpAddr, Window>>,
}

impl RateLimiter {
    pub fn new(max_per_window: u32, window: Duration) -> Self {
        Self {
            max_per_window,
            window,
            windows: Mutex::new(HashMap::new()),
        }
    }

    fn key(ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V4(_) => ip,
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => {
                    let prefix = u128::from(v6) & !u128::from(u64::MAX);
                    IpAddr::V6(Ipv6Addr::from(prefix))
                }
            },
        }
    }

    /// Count one request from `ip` at time `now`. Returns false if the address already
    /// made `max_per_window` requests in its current window, or if
    /// `MAX_TRACKED_ADDRESSES` other addresses have an unexpired window.
    pub fn allow(&self, ip: IpAddr, now: Instant) -> bool {
        let key = Self::key(ip);
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        let expired = |w: &Window| now.saturating_duration_since(w.started) >= self.window;

        if let Some(window) = windows.get_mut(&key) {
            if expired(window) {
                *window = Window {
                    started: now,
                    count: 1,
                };
                return true;
            }
            if window.count >= self.max_per_window {
                return false;
            }
            window.count += 1;
            return true;
        }

        if windows.len() >= MAX_TRACKED_ADDRESSES {
            windows.retain(|_, w| !expired(w));
            if windows.len() >= MAX_TRACKED_ADDRESSES {
                return false;
            }
        }
        windows.insert(
            key,
            Window {
                started: now,
                count: 1,
            },
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn rate_limiter_counts_per_address_and_window() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        let a: IpAddr = "192.0.2.1".parse().unwrap();
        let b: IpAddr = "192.0.2.2".parse().unwrap();
        let t0 = Instant::now();
        assert!(limiter.allow(a, t0));
        assert!(limiter.allow(a, t0));
        assert!(!limiter.allow(a, t0 + Duration::from_secs(59)));
        assert!(limiter.allow(b, t0));
        assert!(limiter.allow(a, t0 + Duration::from_secs(60)));
    }

    #[test]
    fn rate_limiter_groups_ipv6_by_prefix() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let t0 = Instant::now();
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let same_prefix: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let other_prefix: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert!(limiter.allow(a, t0));
        assert!(!limiter.allow(same_prefix, t0));
        assert!(limiter.allow(other_prefix, t0));
    }

    #[test]
    fn client_ip_uses_forwarded_header_only_when_trusted() {
        let remote: SocketAddr = "198.51.100.7:4000".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.5, 192.0.2.9"),
        );
        assert_eq!(client_ip(&headers, remote, false), remote.ip());
        assert_eq!(
            client_ip(&headers, remote, true),
            "192.0.2.9".parse::<IpAddr>().unwrap()
        );
        let mut bad = HeaderMap::new();
        bad.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        assert_eq!(client_ip(&bad, remote, true), remote.ip());
    }
}

#[cfg(test)]
mod serve_tests {
    use super::*;
    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    const BIG_BODY_BYTES: usize = 64 * 1024 * 1024;
    const REQUEST: &[u8] = b"GET / HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n";
    /// Upper bound for every wait in these tests.
    const WAIT: Duration = Duration::from_secs(20);

    struct TestServer {
        addr: SocketAddr,
        stop: oneshot::Sender<()>,
        task: JoinHandle<()>,
    }

    async fn start(limits: ServeLimits) -> TestServer {
        let app = Router::new()
            .route(
                "/",
                get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move {
                    format!("peer={}", peer.ip())
                }),
            )
            .route("/big", get(|| async { vec![b'x'; BIG_BODY_BYTES] }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(serve(listener, app, limits, async move {
            let _ = stopped.await;
        }));
        TestServer { addr, stop, task }
    }

    fn limits() -> ServeLimits {
        ServeLimits {
            max_connections: 16,
            header_read_timeout: Duration::from_secs(10),
            io_idle_timeout: Duration::from_secs(60),
            shutdown_grace: Duration::from_secs(5),
        }
    }

    /// Read until the server closes the connection.
    async fn read_all(stream: &mut TcpStream) -> Vec<u8> {
        let mut out = Vec::new();
        // A reset by the server also ends the read.
        let _ = tokio::time::timeout(WAIT, stream.read_to_end(&mut out))
            .await
            .expect("server did not close the connection");
        out
    }

    #[tokio::test]
    async fn serves_request_with_peer_address_and_stops_on_signal() {
        let server = start(limits()).await;
        let mut stream = TcpStream::connect(server.addr).await.unwrap();
        stream.write_all(REQUEST).await.unwrap();
        let response = String::from_utf8(read_all(&mut stream).await).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.ends_with("peer=127.0.0.1"), "{response}");

        server.stop.send(()).unwrap();
        tokio::time::timeout(WAIT, server.task)
            .await
            .expect("serve did not return after the shutdown signal")
            .unwrap();
        assert!(TcpStream::connect(server.addr).await.is_err());
    }

    #[tokio::test]
    async fn closes_connection_without_complete_header() {
        let server = start(ServeLimits {
            header_read_timeout: Duration::from_millis(200),
            ..limits()
        })
        .await;

        // No byte sent.
        let mut silent = TcpStream::connect(server.addr).await.unwrap();
        // A header that never ends.
        let mut partial = TcpStream::connect(server.addr).await.unwrap();
        partial
            .write_all(b"GET / HTTP/1.1\r\nHost: te")
            .await
            .unwrap();

        let started = Instant::now();
        for stream in [&mut silent, &mut partial] {
            let response = String::from_utf8_lossy(&read_all(stream).await).into_owned();
            assert!(!response.contains("200 OK"), "{response}");
        }
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn serves_at_most_max_connections_at_once() {
        let server = start(ServeLimits {
            max_connections: 1,
            ..limits()
        })
        .await;

        // The first connection holds the only slot without sending a request.
        let first = TcpStream::connect(server.addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut second = TcpStream::connect(server.addr).await.unwrap();
        second.write_all(REQUEST).await.unwrap();
        let mut buf = [0u8; 16];
        let early = tokio::time::timeout(Duration::from_millis(500), second.read(&mut buf)).await;
        assert!(
            early.is_err(),
            "second connection was served while the slot was taken"
        );

        drop(first);
        let response = String::from_utf8(read_all(&mut second).await).unwrap();
        assert!(response.contains("peer=127.0.0.1"), "{response}");
    }

    #[tokio::test]
    async fn closes_connection_whose_client_stops_reading() {
        let server = start(ServeLimits {
            io_idle_timeout: Duration::from_millis(300),
            ..limits()
        })
        .await;

        let mut stream = TcpStream::connect(server.addr).await.unwrap();
        stream
            .write_all(b"GET /big HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        // The client reads nothing, so the server's writes stop once the socket buffers
        // are full. After the idle timeout the server closes the connection.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let received = read_all(&mut stream).await;
        assert!(
            received.len() < BIG_BODY_BYTES,
            "the whole body arrived; the connection was not closed"
        );
    }
}
