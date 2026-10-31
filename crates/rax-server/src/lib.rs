//! The RAX control plane: the `ax.v1alpha1.AX` gRPC service over a resource
//! store, with HTTP health checks on the same port.
//!
//! A task's actor on Agent Substrate is the source of truth for whether the
//! task is running, where, and whether it is healthy. The server reads that
//! through [`Actors`] and drives actors through a [`Reconciler`]; both are
//! implemented on top of the Substrate client.

pub mod controller;
mod http;
pub mod pods;
mod service;
mod tasks;

use async_trait::async_trait;
use rax_api::v1alpha1::{Task, Workspace};
use tonic::{Code, Status};

pub use http::router;
pub use service::{AxService, ServerOptions};

/// The state of a task's actor on Agent Substrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorState {
    Unspecified,
    Running,
    Resuming,
    Reverting,
    Suspending,
    Suspended,
    Crashed,
    Deleting,
    /// A state this version of RAX does not know, by its protobuf number.
    Other(i32),
}

impl ActorState {
    /// The state's protobuf enum name, such as `ACTOR_STATE_RUNNING`.
    pub fn name(self) -> String {
        match self {
            Self::Unspecified => "ACTOR_STATE_UNSPECIFIED".into(),
            Self::Running => "ACTOR_STATE_RUNNING".into(),
            Self::Resuming => "ACTOR_STATE_RESUMING".into(),
            Self::Reverting => "ACTOR_STATE_REVERTING".into(),
            Self::Suspending => "ACTOR_STATE_SUSPENDING".into(),
            Self::Suspended => "ACTOR_STATE_SUSPENDED".into(),
            Self::Crashed => "ACTOR_STATE_CRASHED".into(),
            Self::Deleting => "ACTOR_STATE_DELETING".into(),
            Self::Other(n) => n.to_string(),
        }
    }
}

/// What the server reads from a task's actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub name: String,
    pub state: ActorState,
    /// The pod IP of the worker hosting the actor, or empty when it has none
    /// (for example while it is suspended).
    pub worker_ip: String,
    /// Why the actor crashed, when it did.
    pub crash_message: String,
}

/// Reads actors from Agent Substrate.
#[async_trait]
pub trait Actors: Send + Sync {
    /// Returns the actor, or `None` when it does not exist.
    async fn get_actor(&self, atespace: &str, name: &str) -> Result<Option<Actor>, Status>;
    async fn list_actors(&self, atespace: &str) -> Result<Vec<Actor>, Status>;
    /// The actor to report for a task that has none. `None`, the default,
    /// reports the task as failed: on Substrate a provisioned task always has
    /// an actor.
    fn absent_actor(&self, _name: &str) -> Option<Actor> {
        None
    }
}

/// Coordinates sandbox and actor lifecycles on Agent Substrate.
#[async_trait]
pub trait Reconciler: Send + Sync {
    /// Drives the task's actor toward `task.status.phase` and returns the task
    /// with its new status.
    async fn reconcile(
        &self,
        task: Task,
        workspace: Option<Workspace>,
    ) -> Result<Task, ReconcileError>;
    /// Removes the task's actor and templates from Substrate.
    async fn reconcile_delete(&self, atespace: &str, name: &str) -> Result<(), Status>;
}

/// A failed reconcile, with the task's status as far as it got when known.
#[derive(Debug)]
pub struct ReconcileError {
    pub task: Option<Task>,
    pub status: Status,
}

impl From<Status> for ReconcileError {
    fn from(status: Status) -> Self {
        Self { task: None, status }
    }
}

/// Reports whether a Substrate error is worth retrying: Substrate was
/// unreachable or too slow, rather than refusing the request.
pub fn is_transient(status: &Status) -> bool {
    matches!(status.code(), Code::Unavailable | Code::DeadlineExceeded)
}
