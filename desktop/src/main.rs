//! Polysquish desktop: runs the Polysquish HTTP server in-process on a random loopback port and
//! shows the embedded web UI in a native window backed by the system WebView
//! (WebView2 on Windows, WKWebView on macOS, WebKitGTK on Linux).

// Hide the console window on Windows release builds; keep it in debug for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::{Icon, Window, WindowBuilder};
use wry::{NewWindowResponse, WebView, WebViewBuilder};

#[cfg(target_os = "linux")]
use tao::platform::unix::WindowExtUnix;
#[cfg(target_os = "linux")]
use wry::WebViewBuilderExtUnix;

const WINDOW_TITLE: &str = "Polysquish";
const INITIAL_SIZE: (f64, f64) = (1280.0, 820.0);
const MIN_SIZE: (f64, f64) = (900.0, 600.0);
/// `#0b0716`, the UI's page background, so the window never flashes white.
const BACKGROUND: (u8, u8, u8, u8) = (0x0b, 0x07, 0x16, 0xff);
/// How long to wait for the embedded server to bind before giving up.
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(30);

/// Raw RGBA pixels produced by `build.rs`.
const ICON_RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.rgba"));
const ICON_SIZE: u32 = 256;

fn main() -> Result<()> {
    let output_root = polysquish::server::default_output_root();
    let addr = start_server(output_root)?;
    let url = format!("http://{addr}/");
    eprintln!("Polysquish desktop: server ready at {url}");
    run_window(url)
}

/// Start the Polysquish server on a background thread bound to `127.0.0.1:0` and return the
/// address it ended up listening on.
fn start_server(output_root: PathBuf) -> Result<SocketAddr> {
    let (tx, rx) = mpsc::channel::<Result<SocketAddr, String>>();
    let ready_tx = tx.clone();

    std::thread::Builder::new()
        .name("polysquish-server".into())
        .spawn(move || {
            let outcome = (|| -> Result<()> {
                let rt = tokio::runtime::Runtime::new().context("create tokio runtime")?;
                rt.block_on(polysquish::server::serve_with_ready(0, output_root, None, move |addr| {
                    let _ = ready_tx.send(Ok(addr));
                }))
            })();
            // Either the server failed to start, or it stopped unexpectedly. Both are reported
            // through the same channel; the receiver only cares before the first ready signal.
            let message = match outcome {
                Ok(()) => "server stopped unexpectedly".to_string(),
                Err(e) => format!("{e:#}"),
            };
            let _ = tx.send(Err(message));
        })
        .context("spawn server thread")?;

    match rx.recv_timeout(SERVER_START_TIMEOUT) {
        Ok(Ok(addr)) => Ok(addr),
        Ok(Err(e)) => Err(anyhow!("could not start the Polysquish server: {e}")),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(anyhow!("timed out waiting for the Polysquish server to start")),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!("the Polysquish server thread exited before it was ready")),
    }
}

fn window_icon() -> Option<Icon> {
    Icon::from_rgba(ICON_RGBA.to_vec(), ICON_SIZE, ICON_SIZE).ok()
}

/// `true` for `http(s)://` URLs that do not point at the local machine.
fn is_external_http(url: &str) -> bool {
    let rest = match url.strip_prefix("http://").or_else(|| url.strip_prefix("https://")) {
        Some(rest) => rest,
        None => return false,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Drop any userinfo, then the port.
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split(']').next().unwrap_or(stripped)
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    !matches!(host.to_ascii_lowercase().as_str(), "127.0.0.1" | "localhost" | "::1")
}

fn open_external(url: &str) {
    if let Err(e) = open::that_detached(url) {
        eprintln!("Polysquish desktop: could not open {url} in the system browser: {e}");
    }
}

fn build_webview(window: &Window, url: &str) -> Result<WebView> {
    let builder = WebViewBuilder::new()
        .with_url(url)
        .with_background_color(BACKGROUND)
        // In-page navigation stays inside the app; anything external goes to the system browser.
        .with_navigation_handler(|target| {
            if is_external_http(&target) {
                open_external(&target);
                false
            } else {
                true
            }
        })
        // `window.open` / target="_blank": same policy.
        .with_new_window_req_handler(|target, _features| {
            if is_external_http(&target) {
                open_external(&target);
                NewWindowResponse::Deny
            } else {
                NewWindowResponse::Allow
            }
        });

    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(window).context("create webview")?;

    // On Linux build into the GTK box tao places inside the window so X11 and Wayland both work.
    #[cfg(target_os = "linux")]
    let webview = match window.default_vbox() {
        Some(vbox) => builder.build_gtk(vbox),
        None => builder.build_gtk(window.gtk_window()),
    }
    .context("create webview")?;

    Ok(webview)
}

fn run_window(url: String) -> Result<()> {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(WINDOW_TITLE)
        .with_inner_size(LogicalSize::new(INITIAL_SIZE.0, INITIAL_SIZE.1))
        .with_min_inner_size(LogicalSize::new(MIN_SIZE.0, MIN_SIZE.1))
        .with_background_color(BACKGROUND)
        .with_window_icon(window_icon())
        .build(&event_loop)
        .context("create window")?;

    let webview = build_webview(&window, &url)?;

    event_loop.run(move |event, _target, control_flow| {
        // Keep the window and webview alive for as long as the loop runs.
        let _ = (&window, &webview);
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            // Exiting the process also tears down the server thread.
            *control_flow = ControlFlow::Exit;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::is_external_http;

    #[test]
    fn local_urls_are_not_external() {
        assert!(!is_external_http("http://127.0.0.1:7777/"));
        assert!(!is_external_http("http://127.0.0.1/api/jobs?x=1"));
        assert!(!is_external_http("http://localhost:1234/"));
        assert!(!is_external_http("http://[::1]:1234/"));
        assert!(!is_external_http("about:blank"));
        assert!(!is_external_http("blob:http://127.0.0.1:7777/abc"));
        assert!(!is_external_http("data:text/plain,hi"));
    }

    #[test]
    fn remote_urls_are_external() {
        assert!(is_external_http("https://github.com/"));
        assert!(is_external_http("http://example.com:8080/path"));
        assert!(is_external_http("https://user@example.com/"));
    }
}
