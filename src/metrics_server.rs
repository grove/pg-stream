// OP-2: Minimal stdlib-only OpenMetrics HTTP server for the pg_trickle scheduler.
//
// # Design
//
// - Uses only `std::net::TcpListener` — no runtime dependency.
// - Spawned once when `pg_trickle.metrics_port` is non-zero, after the
//   per-database scheduler finishes startup.
// - The server handles one request per `accept()` call; the caller is
//   responsible for calling `serve_one_request` from the scheduler main loop
//   (non-blocking: `set_nonblocking(true)` is set so accept returns immediately
//   when no connection is pending).
// - Only `GET /metrics` returns data; all other paths get a 404.
// - Metrics are pulled from `monitoring::collect_metrics_text()` on each
//   request — always fresh, no caching.
//
// # OpenMetrics format
//
// Each metric line follows:
//   <metric_name>{label="value",...} <value> [<timestamp_ms>]
//
// # Security
//
// The endpoint is NOT authenticated. Deploy behind a firewall or bind only to
// loopback (`127.0.0.1`) in production. The port is configurable via
// `pg_trickle.metrics_port` (default 0 = disabled).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// Handle for a running metrics HTTP server.
///
/// Drop to stop accepting new connections (the OS will clean up the socket).
pub struct MetricsServer {
    listener: TcpListener,
}

impl MetricsServer {
    /// Start a non-blocking metrics server on `port`.
    ///
    /// Returns `None` when `port == 0` (disabled) or when the bind fails.
    /// Binding failures are logged as warnings — a metrics port conflict must
    /// never prevent the scheduler from starting.
    pub fn start(port: u16) -> Option<Self> {
        if port == 0 {
            return None;
        }
        let addr = format!("127.0.0.1:{port}");
        match TcpListener::bind(&addr) {
            Ok(listener) => {
                if let Err(e) = listener.set_nonblocking(true) {
                    pgrx::warning!(
                        "[pg_trickle] OP-2: failed to set metrics socket non-blocking: {e}"
                    );
                    return None;
                }
                pgrx::log!("[pg_trickle] OP-2: metrics endpoint started on http://{addr}/metrics");
                Some(Self { listener })
            }
            Err(e) => {
                pgrx::warning!(
                    "[pg_trickle] OP-2: failed to bind metrics endpoint on {addr}: {e}; \
                     metrics endpoint will not be available."
                );
                None
            }
        }
    }

    /// Poll for a single pending connection and serve it.
    ///
    /// Returns immediately (non-blocking) when no client is waiting.
    /// Call this once per scheduler tick.
    pub fn serve_one_request(&self, metrics_text: &str) {
        match self.listener.accept() {
            Ok((mut stream, _peer)) => {
                // Set a short read timeout so a slow client cannot stall the scheduler.
                let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
                handle_connection(&mut stream, metrics_text);
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // No connection pending — expected fast path.
            }
            Err(e) => {
                pgrx::warning!("[pg_trickle] OP-2: metrics accept error: {e}");
            }
        }
    }
}

/// Read the first line of an HTTP request and respond.
fn handle_connection(stream: &mut TcpStream, metrics_text: &str) {
    let mut buf = [0u8; 1024];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return,
    };
    let request = std::str::from_utf8(&buf[..n]).unwrap_or("");

    // Only serve GET /metrics — everything else gets a 404.
    let (status, content_type, body) = if request.starts_with("GET /metrics") {
        (
            "200 OK",
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
            metrics_text,
        )
    } else if request.starts_with("GET /health") || request.starts_with("GET /-/healthy") {
        ("200 OK", "text/plain", "ok\n")
    } else {
        ("404 Not Found", "text/plain", "Not Found\n")
    };

    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}
