use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::{Bytes, Incoming};
use hyper::service::{Service, service_fn};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use rand::Rng;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use tempo_agentic_mcp::AdminHandler;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

type AdminService = StreamableHttpService<AdminHandler, LocalSessionManager>;

/// The admin surface, listening on loopback, plus the file that says how to
/// reach it.
///
/// Dropping this stops the server and removes the manifest, so a stale file
/// never points at a port nobody is on.
pub struct AdminServer {
    pub url: String,
    manifest: PathBuf,
    task: JoinHandle<()>,
}

impl AdminServer {
    /// Starts the server and publishes its address and token beside `database`.
    ///
    /// Returns an error if loopback cannot be bound or the manifest cannot be
    /// written.
    pub async fn start(handler: AdminHandler, database: &Path) -> Result<Self> {
        // Port zero lets the system pick: a fixed one would collide with a second
        // daemon running against a different database.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("cannot bind the admin server to loopback")?;
        let port = listener
            .local_addr()
            .context("cannot read the admin server port")?
            .port();
        let token = random_token();
        let url = format!("http://127.0.0.1:{port}/");

        let manifest = manifest_path(database);
        write_manifest(&manifest, &url, &token)?;

        let service = StreamableHttpService::new(
            move || Ok(handler.clone()),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default()
                // Request and response, no long-lived sessions: this surface is
                // a handful of reads and one write, and plain JSON keeps it
                // debuggable with curl.
                .with_stateful_mode(false)
                .with_json_response(true),
        );

        Ok(Self {
            url,
            manifest,
            task: tokio::spawn(accept(listener, service, token)),
        })
    }
}

impl Drop for AdminServer {
    fn drop(&mut self) {
        self.task.abort();
        if let Err(error) = std::fs::remove_file(&self.manifest) {
            tracing::warn!(path = %self.manifest.display(), %error, "cannot remove the admin manifest");
        }
    }
}

/// Where the manifest for a database lives.
pub fn manifest_path(database: &Path) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(".mcp.json");
    PathBuf::from(path)
}

async fn accept(listener: TcpListener, service: AdminService, token: String) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => {
                tracing::warn!(%error, "admin server cannot accept a connection");
                continue;
            }
        };
        let inner = TowerToHyperService::new(service.clone());
        let token = token.clone();
        tokio::spawn(async move {
            let guarded = service_fn(move |request: Request<Incoming>| {
                let inner = inner.clone();
                let token = token.clone();
                async move {
                    if !authorized(&request, &token) {
                        return Ok(unauthorized());
                    }
                    inner.call(request).await
                }
            });
            if let Err(error) = auto::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), guarded)
                .await
            {
                tracing::debug!(%error, "admin connection closed");
            }
        });
    }
}

// Checked before the request reaches rmcp, because `set_level` stores rules that
// spend funds and loopback alone only keeps other machines out, not other
// processes on this one.
//
// Plain equality on purpose: the secret is 256 random bits over a local socket,
// so a timing attack has nothing to work with. Do not "fix" this into something
// that leaks length by comparing prefixes.
fn authorized(request: &Request<Incoming>, token: &str) -> bool {
    request
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|supplied| supplied == token)
}

fn unauthorized() -> Response<BoxBody<Bytes, Infallible>> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(
            b"the admin surface needs the bearer token from the manifest file",
        ))
        .boxed(),
    );
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response
}

fn random_token() -> String {
    let bytes: [u8; 32] = rand::thread_rng().r#gen();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_manifest(path: &Path, url: &str, token: &str) -> Result<()> {
    let body = serde_json::json!({ "url": url, "token": token });
    std::fs::write(path, serde_json::to_vec_pretty(&body)?)
        .with_context(|| format!("cannot write the admin manifest {}", path.display()))?;
    // The token is the only thing between another user's process and a rule that
    // spends funds, so the file it lives in is readable by its owner alone.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("cannot restrict the admin manifest {}", path.display()))?;
    }
    Ok(())
}
