//! `rax-server`: the RAX control plane binary.

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use rax_server::controller::{ControllerOptions, SubstrateActors, TaskReconciler};
use rax_server::{AxService, ServerOptions, router};
use rax_store::{
    Locker, MemoryLocker, MemoryStore, PostgresOptions, PostgresStore, Store, describe_url,
};
use rax_substrate::ClientOptions;

/// Serves the AX gRPC API and `/healthz` on one port.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Listen address. Like Go, `:8080` listens on every interface.
    #[arg(long, env = "ADDR", default_value = "0.0.0.0:8080")]
    addr: String,

    /// PostgreSQL connection URL. libpq variables such as PGPASSWORD fill in
    /// what it leaves out. `memory://` keeps everything in this process, for
    /// local runs and tests.
    #[arg(
        long,
        env = "AX_STORE",
        default_value = "postgres://ax@localhost:5432/ax?sslmode=disable"
    )]
    store: String,

    /// Agent Substrate control API endpoint.
    #[arg(long, default_value = rax_substrate::DEFAULT_TARGET)]
    substrate_endpoint: String,

    /// Authority and TLS server name for the Substrate endpoint.
    #[arg(long, default_value = "api.ate-system.svc")]
    substrate_authority: String,

    /// Bearer token file for Substrate. Defaults to SUBSTRATE_TOKEN_FILE or
    /// the mounted service account token.
    #[arg(long, default_value = "", hide_default_value = true)]
    substrate_token_file: String,

    /// CA PEM file for Substrate TLS. Defaults to SUBSTRATE_CA_FILE or the
    /// mounted trust bundle.
    #[arg(long, default_value = "", hide_default_value = true)]
    substrate_ca_file: String,

    /// Skip verifying Substrate's TLS certificate.
    #[arg(long)]
    substrate_insecure_tls: bool,

    /// Connect to Substrate over plaintext gRPC.
    #[arg(long)]
    substrate_plaintext: bool,

    /// Default Substrate ActorTemplate, as `name` or `atespace/name`.
    #[arg(long, default_value = "default-template")]
    template: String,

    /// Atespace of the default ActorTemplate.
    #[arg(long, default_value = rax_api::DEFAULT_SYSTEM_NAMESPACE)]
    template_atespace: String,

    /// Container image for tasks that do not set spec.image.
    #[arg(long, env = "AX_DEFAULT_TASK_IMAGE", default_value = rax_api::DEFAULT_TASK_IMAGE)]
    default_task_image: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tokio_postgres=warn".into()),
        )
        .init();
    if let Err(e) = run(Args::parse()).await {
        tracing::error!("{e}");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let (store, locker): (Arc<dyn Store>, Arc<dyn Locker>) = if args.store == "memory://" {
        tracing::warn!(
            "using the in-memory store: state is lost on exit and not shared between replicas"
        );
        (Arc::new(MemoryStore::new()), Arc::new(MemoryLocker::new()))
    } else {
        tracing::info!(store = describe_url(&args.store), "opening store");
        let pg = tokio::time::timeout(
            Duration::from_secs(60),
            PostgresStore::open(&args.store, PostgresOptions::default()),
        )
        .await
        .map_err(|_| format!("opening store {}: timed out", describe_url(&args.store)))?
        .map_err(|e| format!("opening store {}: {e}", describe_url(&args.store)))?;
        let pg = Arc::new(pg);
        (pg.clone(), pg)
    };

    tracing::info!(
        substrate_endpoint = args.substrate_endpoint,
        template = args.template,
        default_task_image = args.default_task_image,
        "connecting to substrate"
    );
    let client = rax_substrate::Client::connect(ClientOptions {
        target: args.substrate_endpoint,
        authority: args.substrate_authority,
        token_file: args.substrate_token_file,
        ca_file: args.substrate_ca_file,
        insecure_tls: args.substrate_insecure_tls,
        plaintext: args.substrate_plaintext,
    });
    let mut opts = ServerOptions {
        locker: Some(locker),
        ..Default::default()
    };
    match client {
        Ok(client) => {
            let reconciler = TaskReconciler::new(
                client.clone(),
                ControllerOptions {
                    default_template: args.template,
                    default_template_atespace: args.template_atespace,
                    default_task_image: args.default_task_image,
                    router_addr: std::env::var("ATENET_ROUTER_ADDR").unwrap_or_default(),
                    ..Default::default()
                },
            );
            opts.reconciler = Some(Arc::new(reconciler));
            opts.actors = Some(Arc::new(SubstrateActors::new(client)));
        }
        Err(e) => tracing::warn!(
            error = %e,
            "could not initialize substrate client; running without substrate reconciliation"
        ),
    }
    let service = AxService::new(store, opts);

    let listener = tokio::net::TcpListener::bind(listen_addr(&args.addr)).await?;
    tracing::info!(addr = %listener.local_addr()?, "starting rax-server");
    axum::serve(listener, router(service))
        .with_graceful_shutdown(shutdown())
        .await?;
    tracing::info!("shut down rax-server");
    Ok(())
}

/// Accepts Go's host-less `:port` form, which AX's manifests pass as
/// `--addr=:8080`, as well as anything `TcpListener::bind` takes.
fn listen_addr(addr: &str) -> String {
    match addr.strip_prefix(':') {
        Some(port) => format!("0.0.0.0:{port}"),
        None => addr.to_string(),
    }
}

async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing the SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}

#[cfg(test)]
mod tests {
    use super::listen_addr;

    #[test]
    fn listen_addr_accepts_go_port_form() {
        assert_eq!(listen_addr(":8080"), "0.0.0.0:8080");
        assert_eq!(listen_addr("127.0.0.1:8080"), "127.0.0.1:8080");
        assert_eq!(listen_addr("[::]:8080"), "[::]:8080");
    }
}
