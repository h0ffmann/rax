//! `rax-server`: the RAX control plane binary.

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use rax_server::{AxService, ServerOptions, router};
use rax_store::{
    Locker, MemoryLocker, MemoryStore, PostgresOptions, PostgresStore, Store, describe_url,
};

/// Serves the AX gRPC API and `/healthz` on one port.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Listen address.
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

    // Substrate reconciliation arrives with the Substrate client; until then
    // tasks are stored and reported as stored.
    tracing::warn!("running without substrate reconciliation");
    let service = AxService::new(
        store,
        ServerOptions {
            locker: Some(locker),
            ..Default::default()
        },
    );

    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    tracing::info!(addr = %listener.local_addr()?, "starting rax-server");
    axum::serve(listener, router(service))
        .with_graceful_shutdown(shutdown())
        .await?;
    tracing::info!("shut down rax-server");
    Ok(())
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
