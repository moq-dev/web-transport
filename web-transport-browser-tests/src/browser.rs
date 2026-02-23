use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::browser::BrowserContextId;
use chromiumoxide::cdp::browser_protocol::target::{
    CreateBrowserContextParams, CreateTargetParams,
};
use chromiumoxide::fetcher::{BrowserFetcher, BrowserFetcherOptions};
use chromiumoxide::Page;
use futures::StreamExt;
use serde::Deserialize;

use crate::js;

struct SharedBrowser {
    browser: Browser,
    /// URL of the blank page server (http://localhost:{port}).
    page_url: String,
    // A dedicated Tokio runtime that owns the browser event handler and HTTP
    // server tasks.  This runtime lives as long as the `SharedBrowser` (i.e.
    // for the entire process) so that the tasks are not cancelled when
    // individual `#[tokio::test]` runtimes shut down.
    _runtime: tokio::runtime::Runtime,
    // A page that stays open for the lifetime of the browser to prevent
    // Chrome from exiting when all test contexts are disposed.
    _keepalive_page: Page,
    // Holds the stdin pipe to the cleanup watchdog process.  When our process
    // exits (for any reason, including SIGKILL), the OS closes this FD,
    // unblocking the watchdog which then kills Chrome and removes the temp dir.
    _watchdog_pipe: Option<std::process::ChildStdin>,
}

// Safety: Browser and Page are Send + Sync; the runtime is only used to keep
// background tasks alive and is never accessed from multiple threads.
unsafe impl Sync for SharedBrowser {}

static BROWSER: OnceLock<SharedBrowser> = OnceLock::new();

/// Try to build a BrowserConfig, auto-downloading Chromium via the fetcher if
/// no local executable is detected.
async fn build_browser_config() -> BrowserConfig {
    // Use a per-process data dir to avoid stale SingletonLock conflicts.
    let data_dir = std::env::temp_dir().join(format!("chromiumoxide-{}", std::process::id()));

    // Note: .arg() auto-prefixes with "--", so pass bare names.
    let builder = BrowserConfig::builder()
        .new_headless_mode()
        .no_sandbox()
        .user_data_dir(&data_dir)
        .arg("allow-insecure-localhost");

    match builder.clone().build() {
        Ok(config) => return config,
        Err(_) => {
            tracing::info!("no local Chrome found, downloading via fetcher...");
        }
    }

    // Auto-detection failed — download Chromium.
    let exe_path = fetch_chromium().await;

    builder
        .chrome_executable(exe_path)
        .build()
        .expect("failed to build browser config with fetched executable")
}

/// Download Chromium using chromiumoxide_fetcher and return the executable path.
async fn fetch_chromium() -> PathBuf {
    // Use the same cache directory as the fetcher's default
    // ($XDG_CACHE_HOME or $HOME/.cache)/chromiumoxide. We must create it
    // ourselves because the fetcher doesn't create parent directories.
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").expect("HOME not set")).join(".cache")
        });
    let cache_dir = base.join("chromiumoxide");
    std::fs::create_dir_all(&cache_dir).expect("failed to create browser cache directory");

    let options = BrowserFetcherOptions::builder()
        .with_path(&cache_dir)
        .build()
        .expect("failed to create fetcher options");

    let fetcher = BrowserFetcher::new(options);
    let installation = fetcher.fetch().await.expect("failed to download Chromium");

    tracing::info!(path = %installation.executable_path.display(), "downloaded Chromium");
    installation.executable_path
}

/// Spawn a background shell that blocks on stdin.  When our process exits (for
/// any reason), the OS closes the pipe, `read` gets EOF, and the shell kills
/// Chrome and removes its data directory.
fn spawn_cleanup_watchdog(
    chrome_pid: u32,
    data_dir: &std::path::Path,
) -> Option<std::process::ChildStdin> {
    use std::process::{Command, Stdio};

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "read _; kill -9 {} 2>/dev/null; rm -rf '{}'",
            chrome_pid,
            data_dir.display(),
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()
}

fn init_browser() -> SharedBrowser {
    // Create a dedicated runtime on a separate thread.  We cannot call
    // `block_on` from within an existing Tokio runtime (which `#[tokio::test]`
    // creates), so we spawn a plain OS thread to build and initialise
    // everything, then send the results back.
    std::thread::spawn(|| {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to build dedicated browser runtime");

        let (browser, page_url, keepalive_page, chrome_pid) = rt.block_on(async {
            let config = build_browser_config().await;

            // Start the blank page server on this runtime.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("failed to bind blank page server");
            let addr: SocketAddr = listener.local_addr().unwrap();
            let page_url = format!("http://localhost:{}", addr.port());

            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        break;
                    };
                    tokio::spawn(async move {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        let mut buf = [0u8; 1024];
                        let _ = stream.read(&mut buf).await;
                        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 13\r\n\r\n<html></html>";
                        let _ = stream.write_all(response.as_bytes()).await;
                    });
                }
            });

            let (mut browser, mut handler) = Browser::launch(config)
                .await
                .expect("failed to launch browser");

            // Extract Chrome's PID before we lose mutability, so the watchdog
            // can kill it on abnormal exit.
            let chrome_pid = browser
                .get_mut_child()
                .and_then(|c| c.inner.id())
                .unwrap_or(0);

            // Spawn the CDP event handler on this runtime.
            tokio::spawn(async move {
                while let Some(event) = handler.next().await {
                    let _ = event;
                }
            });

            // Open a keepalive page in the default context so Chrome doesn't
            // exit when all test browser contexts are disposed.
            let keepalive_page = browser
                .new_page(&page_url)
                .await
                .expect("failed to create keepalive page");

            (browser, page_url, keepalive_page, chrome_pid)
        });

        // Spawn a watchdog that kills Chrome and cleans up the temp dir when
        // our process exits.  The data dir path matches build_browser_config().
        let data_dir =
            std::env::temp_dir().join(format!("chromiumoxide-{}", std::process::id()));
        let watchdog_pipe = spawn_cleanup_watchdog(chrome_pid, &data_dir);

        SharedBrowser {
            browser,
            page_url,
            _runtime: rt,
            _keepalive_page: keepalive_page,
            _watchdog_pipe: watchdog_pipe,
        }
    })
    .join()
    .expect("browser init thread panicked")
}

fn get_browser() -> &'static SharedBrowser {
    BROWSER.get_or_init(init_browser)
}

/// An isolated browser context for a single test.
///
/// Each context has its own cookies, cache, and page — preventing
/// cross-test interference.
pub struct TestContext {
    page: Page,
    context_id: Option<BrowserContextId>,
}

impl TestContext {
    /// Create a new isolated browser context.
    pub async fn new() -> Result<Self> {
        let shared = get_browser();

        let context_id = shared
            .browser
            .create_browser_context(CreateBrowserContextParams::default())
            .await
            .context("failed to create browser context")?;

        // Navigate to a localhost HTTP page so the JS context has a secure
        // origin with the WebTransport API available.
        let page = shared
            .browser
            .new_page(
                CreateTargetParams::builder()
                    .url(&shared.page_url)
                    .browser_context_id(context_id.clone())
                    .build()
                    .unwrap(),
            )
            .await
            .context("failed to create page")?;

        Ok(Self {
            page,
            context_id: Some(context_id),
        })
    }

    /// Evaluate a JavaScript test snippet in the browser.
    ///
    /// The `js_code` is wrapped with `connectWebTransport()` and error handling
    /// via [`js::wrap_test_js`].
    pub async fn run_js_test(
        &self,
        server_url: &str,
        certificate_hash: &[u8],
        js_code: &str,
        timeout: Duration,
    ) -> Result<JsTestResult> {
        let wrapped = js::wrap_test_js(server_url, certificate_hash, js_code);

        let result: String = tokio::time::timeout(timeout, self.page.evaluate(wrapped))
            .await
            .context("JS test timed out")?
            .context("JS evaluation failed")?
            .into_value()
            .context("failed to extract JS result value")?;

        let parsed: JsTestResult =
            serde_json::from_str(&result).context("failed to parse JS test result")?;

        Ok(parsed)
    }

    /// Dispose of the browser context.
    pub async fn dispose(mut self) {
        if let Some(id) = self.context_id.take() {
            let shared = get_browser();
            let _ = shared.browser.dispose_browser_context(id).await;
        }
    }
}

/// The result of a JavaScript test evaluation.
#[derive(Debug, Deserialize)]
pub struct JsTestResult {
    pub success: bool,
    pub message: String,
    #[serde(default)]
    pub details: serde_json::Value,
}
