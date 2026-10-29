//! The guest data plane that runs inside a task's container, and a client for
//! it.
//!
//! The server half is a port of Agent Substrate's `env/guest` package, which
//! AX's task-runner embeds: a process service (start a command, stream its
//! spooled output, feed its stdin, signal its process group) and a filesystem
//! service (stream files in and out). The client half is a port of AX's
//! `internal/guest`, which `ax ssh` uses to run a command in a task.

pub mod client;
pub mod filesystem;
pub mod process;
mod signal;

pub mod proto {
    //! Types and services generated from `proto/ateenv/v1alpha/guest.proto`.
    #![allow(clippy::all)]
    tonic::include_proto!("ateenv.v1alpha");

    /// Encoded `FileDescriptorSet` for guest.proto and its imports, for
    /// gRPC reflection.
    pub const FILE_DESCRIPTOR_SET: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/guest_descriptor.bin"));
}

pub use client::{Client, ExecOptions};
pub use signal::{from_syscall_signal, to_syscall_signal};

use std::collections::HashMap;
use std::path::PathBuf;

use proto::file_system_service_server::FileSystemServiceServer;
use proto::process_service_server::ProcessServiceServer;
use tonic::service::Routes;

/// Configures the guest services.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where process output is spooled. Empty picks `$LOG_DIR`, then
    /// `/var/log/ate-jobs`, then `ate-jobs` under the temp directory.
    pub log_dir: Option<PathBuf>,
    /// The working directory of processes, and the directory file access is
    /// confined to. Empty means the guest's own working directory and no
    /// confinement.
    pub workspace: Option<PathBuf>,
    /// Added to the environment of every process the guest starts.
    pub env: HashMap<String, String>,
    pub enable_process: bool,
    pub enable_filesystem: bool,
}

impl Default for Config {
    /// Both services enabled, with the default log directory.
    fn default() -> Self {
        Self {
            log_dir: None,
            workspace: None,
            env: HashMap::new(),
            enable_process: true,
            enable_filesystem: true,
        }
    }
}

/// The guest services as gRPC routes, plus reflection, ready to be served
/// alone or next to other HTTP handlers. The returned tracker owns the
/// processes started through the routes; dropping it stops pruning old ones.
pub fn routes(cfg: &Config) -> std::io::Result<(Routes, Option<process::Tracker>)> {
    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(proto::FILE_DESCRIPTOR_SET)
        .build_v1()
        .map_err(std::io::Error::other)?;
    let mut routes = Routes::new(reflection);

    let mut tracker = None;
    if cfg.enable_process {
        let mut tc = process::TrackerConfig::with_log_dir(cfg.log_dir.clone());
        tc.workspace = cfg.workspace.clone();
        tc.env = cfg.env.clone();
        let t = process::Tracker::new(tc)?;
        routes = routes.add_service(ProcessServiceServer::new(process::Service::new(t.clone())));
        tracker = Some(t);
    }
    if cfg.enable_filesystem {
        routes = routes.add_service(FileSystemServiceServer::new(filesystem::Service::new(
            filesystem::Config {
                root_directory: cfg.workspace.clone(),
                ..Default::default()
            },
        )));
    }
    Ok((routes, tracker))
}
