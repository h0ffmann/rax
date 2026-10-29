//! The metadata and guest server inside a task's container.
//!
//! It runs two listeners. The probe listener, on all interfaces, serves
//! `/healthz` and `/readyz`, and with `spec.debug` the guest gRPC services on
//! the same port. The metadata listener serves the task and its workspace
//! as YAML on loopback only, so only the container itself can read them.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rax_api::v1alpha1::{Task, Workspace};
use rax_api::{Manifest, to_yaml};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tower::ServiceExt;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Optional settings for the server.
#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    /// The working directory of guest processes and the root of guest file
    /// access.
    pub workspace_path: Option<String>,
    /// Where guest process output is spooled.
    pub log_dir: Option<PathBuf>,
    /// The loopback port for the metadata endpoints. Zero picks a free one.
    pub metadata_port: u16,
    /// Added to the environment of guest processes, along with
    /// `AX_METADATA_URL`.
    pub guest_env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Default)]
struct Shared {
    task: RwLock<Option<Task>>,
    workspace: RwLock<Option<Workspace>>,
    workspace_ready: AtomicBool,
}

/// A running metadata and guest server.
#[derive(Debug)]
pub struct Server {
    shared: Arc<Shared>,
    probe_addr: SocketAddr,
    metadata_addr: SocketAddr,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    // Keeps pruning guest processes while the server runs.
    _tracker: Option<rax_guest::process::Tracker>,
}

impl Server {
    /// Binds both listeners and starts serving. A zero `port` picks a free
    /// one; read it back with [`Server::probe_addr`].
    pub async fn start(
        port: u16,
        task: Option<Task>,
        workspace: Option<Workspace>,
        opts: ServerOptions,
    ) -> std::io::Result<Self> {
        let debug = task
            .as_ref()
            .and_then(|t| t.spec.as_ref())
            .is_some_and(|s| s.debug);
        let shared = Arc::new(Shared {
            task: RwLock::new(task),
            workspace: RwLock::new(workspace),
            workspace_ready: AtomicBool::new(false),
        });

        let probe_lis = bind_all(port).await.map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("probe and guest server listening on :{port}: {e}"),
            )
        })?;
        let metadata_lis = TcpListener::bind((Ipv4Addr::LOCALHOST, opts.metadata_port))
            .await
            .map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!(
                        "metadata server listening on 127.0.0.1:{}: {e}",
                        opts.metadata_port
                    ),
                )
            })?;
        let probe_addr = probe_lis.local_addr()?;
        let metadata_addr = metadata_lis.local_addr()?;
        // Guest services run processes and touch files in the container, so
        // they are served only when the task opts in with spec.debug.
        let mut grpc = None;
        let mut tracker = None;
        if debug {
            let cfg = rax_guest::Config {
                log_dir: opts.log_dir.clone(),
                workspace: opts.workspace_path.as_ref().map(PathBuf::from),
                env: guest_env(&opts.guest_env, metadata_addr),
                ..Default::default()
            };
            match rax_guest::routes(&cfg) {
                Ok((routes, t)) => {
                    grpc = Some(routes.prepare().into_axum_router());
                    tracker = t;
                    tracing::info!("guest services enabled (spec.debug is true)");
                }
                Err(e) => tracing::error!(error = %e, "failed to initialize guest gRPC server"),
            }
        } else {
            tracing::info!(
                "guest services disabled; set spec.debug: true on the Task to enable ax ssh"
            );
        }

        let probe = Router::new()
            .route("/healthz", get(|| async { "ok\n" }))
            .route("/readyz", get(readyz))
            .with_state(shared.clone())
            .fallback(move |req: Request| async move {
                match grpc {
                    Some(grpc) if is_grpc(&req) => grpc.oneshot(req).await.into_response(),
                    _ => not_found(),
                }
            });
        let metadata = Router::new()
            .route("/metadata/v1alpha1/ax/task", get(task_yaml))
            .route("/metadata/v1alpha1/ax/workspace", get(workspace_yaml))
            .fallback(|| async { not_found() })
            .with_state(shared.clone());

        tracing::info!(addr = %probe_addr, "probe and guest server started");
        tracing::info!(addr = %metadata_addr, "metadata server started");

        let (shutdown, _) = watch::channel(false);
        let tasks = [(probe_lis, probe, "probe and guest"), (metadata_lis, metadata, "metadata")]
            .into_iter()
            .map(|(lis, router, name)| {
                let mut stop = shutdown.subscribe();
                tokio::spawn(async move {
                    let serve = axum::serve(lis, router).with_graceful_shutdown(async move {
                        let _ = stop.wait_for(|s| *s).await;
                    });
                    if let Err(e) = serve.await {
                        tracing::error!(error = %e, "{name} server error");
                    }
                })
            })
            .collect();

        Ok(Self {
            shared,
            probe_addr,
            metadata_addr,
            shutdown,
            tasks,
            _tracker: tracker,
        })
    }

    /// Where the probe and guest listener is bound.
    pub fn probe_addr(&self) -> SocketAddr {
        self.probe_addr
    }

    /// The base URL of the metadata endpoints, as the task command gets it in
    /// `AX_METADATA_URL`.
    pub fn metadata_url(&self) -> String {
        format!("http://{}", self.metadata_addr)
    }

    /// Replaces the task and workspace the metadata endpoints serve.
    pub fn update_state(&self, task: Option<Task>, workspace: Option<Workspace>) {
        *self.shared.task.write().unwrap() = task;
        *self.shared.workspace.write().unwrap() = workspace;
    }

    /// Records whether first-run workspace setup has finished, which
    /// `/readyz` reports.
    pub fn set_workspace_ready(&self, ready: bool) {
        self.shared.workspace_ready.store(ready, Ordering::SeqCst);
    }

    pub fn is_workspace_ready(&self) -> bool {
        self.shared.workspace_ready.load(Ordering::SeqCst)
    }

    /// Stops both listeners, letting open requests finish for a few seconds.
    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        for task in self.tasks {
            let abort = task.abort_handle();
            if tokio::time::timeout(SHUTDOWN_GRACE, task).await.is_err() {
                abort.abort();
            }
        }
    }
}

fn guest_env(
    extra: &std::collections::HashMap<String, String>,
    metadata_addr: SocketAddr,
) -> std::collections::HashMap<String, String> {
    let mut env = extra.clone();
    env.insert("AX_METADATA_URL".into(), format!("http://{metadata_addr}"));
    env
}

/// Listens on every interface, IPv6 and IPv4 when the host has IPv6.
async fn bind_all(port: u16) -> std::io::Result<TcpListener> {
    match TcpListener::bind((Ipv6Addr::UNSPECIFIED, port)).await {
        Ok(lis) => Ok(lis),
        Err(_) => TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await,
    }
}

fn is_grpc(req: &Request) -> bool {
    req.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/grpc"))
}

/// A plain-text error the way Go's `http.Error` writes one.
fn text_error(status: StatusCode, msg: &str) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        format!("{msg}\n"),
    )
        .into_response()
}

fn not_found() -> Response {
    text_error(StatusCode::NOT_FOUND, "404 page not found")
}

async fn readyz(State(shared): State<Arc<Shared>>) -> Response {
    if shared.workspace_ready.load(Ordering::SeqCst) {
        "ok\n".into_response()
    } else {
        text_error(StatusCode::SERVICE_UNAVAILABLE, "workspace initializing")
    }
}

fn yaml_response<T: Manifest>(resource: Option<&T>, missing: &str) -> Response {
    let Some(resource) = resource else {
        return text_error(StatusCode::NOT_FOUND, missing);
    };
    match to_yaml(resource) {
        Ok(body) => ([(header::CONTENT_TYPE, "application/yaml")], body).into_response(),
        Err(e) => text_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn task_yaml(State(shared): State<Arc<Shared>>) -> Response {
    yaml_response(
        shared.task.read().unwrap().as_ref(),
        "task metadata not available",
    )
}

/// The bound workspace, or 404 when the task binds none.
async fn workspace_yaml(State(shared): State<Arc<Shared>>) -> Response {
    yaml_response(
        shared.workspace.read().unwrap().as_ref(),
        "workspace metadata not available",
    )
}
