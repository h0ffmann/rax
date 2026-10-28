//! The `ax.v1alpha1.AX` gRPC service.

use std::future::Future;
use std::sync::Arc;

use rax_api::phase;
use rax_api::v1alpha1::{self as api, ObjectMeta, Task, TaskStatus, Workspace, ax_server::Ax};
use rax_store::{DEFAULT_ATESPACE, Locker, MemoryLocker, ResourceStore, Store, StoreError};
use tonic::{Request, Response, Status};

use crate::tasks::{Tasks, now};
use crate::{Actors, Reconciler, is_transient};

#[derive(Default, Clone)]
pub struct ServerOptions {
    /// Serializes changes to a resource. It must be shared by every server
    /// replica; it defaults to an in-process [`MemoryLocker`].
    pub locker: Option<Arc<dyn Locker>>,
    /// Drives task actors on Substrate. Without it, tasks are only stored.
    pub reconciler: Option<Arc<dyn Reconciler>>,
    /// Reads the parts of a task's status owned by its actor, which the store
    /// never holds. Without it, tasks are returned as stored.
    pub actors: Option<Arc<dyn Actors>>,
}

/// The AX API. Its methods can be called directly, or served over gRPC
/// through the [`Ax`] trait.
#[derive(Clone)]
pub struct AxService {
    tasks: Tasks,
    resources: Arc<dyn ResourceStore>,
    locker: Arc<dyn Locker>,
    reconciler: Option<Arc<dyn Reconciler>>,
}

fn atespace_or_default(atespace: &str) -> String {
    if atespace.is_empty() {
        DEFAULT_ATESPACE.into()
    } else {
        atespace.into()
    }
}

fn not_found(kind: &str, name: &str, atespace: &str) -> Status {
    Status::not_found(format!(
        "{kind} {name:?} not found in atespace {atespace:?}"
    ))
}

/// Maps a store error from a read to NotFound or Internal.
fn read_error(kind: &str, name: &str, atespace: &str) -> impl FnOnce(StoreError) -> Status {
    move |e| match e {
        StoreError::NotFound => not_found(kind, name, atespace),
        e => Status::internal(format!("getting {kind}: {e}")),
    }
}

fn internal(context: &str) -> impl FnOnce(StoreError) -> Status + '_ {
    move |e| Status::internal(format!("{context}: {e}"))
}

impl AxService {
    pub fn new(store: Arc<dyn Store>, opts: ServerOptions) -> Self {
        Self {
            tasks: Tasks::new(store.clone(), opts.actors),
            resources: store,
            locker: opts.locker.unwrap_or_else(|| Arc::new(MemoryLocker::new())),
            reconciler: opts.reconciler,
        }
    }

    /// Runs `f` holding the exclusive lock for one resource.
    async fn locked<T>(
        &self,
        kind: &str,
        atespace: &str,
        name: &str,
        f: impl Future<Output = Result<T, Status>>,
    ) -> Result<T, Status> {
        let guard = self
            .locker
            .lock(kind, atespace, name)
            .await
            .map_err(|e| Status::aborted(format!("locking {kind} {atespace}/{name}: {e}")))?;
        let out = f.await;
        guard.release().await;
        out
    }

    // --- Tasks ---

    pub async fn get_task(&self, req: api::GetTaskRequest) -> Result<Task, Status> {
        let atespace = atespace_or_default(&req.atespace);
        self.tasks
            .get(&atespace, &req.name)
            .await
            .map_err(read_error("task", &req.name, &atespace))
    }

    pub async fn list_tasks(
        &self,
        req: api::ListTasksRequest,
    ) -> Result<api::ListTasksResponse, Status> {
        let limit = if req.limit > 0 {
            req.limit
        } else {
            rax_store::DEFAULT_TASK_LIST_LIMIT
        };
        let offset = req.offset.max(0);
        let tasks = self
            .tasks
            .list(&req.atespace, limit, offset)
            .await
            .map_err(internal("listing tasks"))?;
        Ok(api::ListTasksResponse { tasks })
    }

    pub async fn create_task(&self, req: api::CreateTaskRequest) -> Result<Task, Status> {
        let mut task = req
            .task
            .ok_or_else(|| Status::invalid_argument("task required"))?;
        rax_api::validate_task(&task).map_err(|e| Status::invalid_argument(e.to_string()))?;
        let meta = task.metadata.get_or_insert_with(Default::default);
        if meta.atespace.is_empty() {
            meta.atespace = DEFAULT_ATESPACE.into();
        }
        let (atespace, name) = (meta.atespace.clone(), meta.name.clone());
        self.locked(
            "task",
            &atespace,
            &name,
            self.create_task_locked(task, &atespace, &name),
        )
        .await
    }

    async fn create_task_locked(
        &self,
        mut task: Task,
        atespace: &str,
        name: &str,
    ) -> Result<Task, Status> {
        match self.tasks.get(atespace, name).await {
            Ok(existing) => {
                // Only a task that never got an actor may be applied again,
                // and only unchanged. Its stored status is kept.
                let pending = existing
                    .status
                    .as_ref()
                    .is_some_and(|s| s.phase == phase::PENDING);
                if !pending || existing.spec != task.spec {
                    return Err(Status::failed_precondition(format!(
                        "task {atespace}/{name} already exists and is immutable"
                    )));
                }
                task = existing;
            }
            Err(StoreError::NotFound) => {}
            Err(e) => return Err(Status::internal(format!("checking existing task: {e}"))),
        }

        let meta = task.metadata.as_mut().expect("metadata was defaulted");
        if meta.create_time.is_none() {
            meta.create_time = Some(now());
        }
        // The task is Pending until its actor exists.
        task.status.get_or_insert_with(Default::default).phase = phase::PENDING.into();
        self.tasks
            .save(&task)
            .await
            .map_err(internal("saving task"))?;
        // New tasks start suspended.
        task.status.as_mut().unwrap().phase = phase::SUSPENDED.into();

        if let Some(r) = &self.reconciler {
            let ws = self.fetch_workspace(atespace, &task).await;
            match r.reconcile(task.clone(), ws).await {
                Ok(reconciled) => task.status = reconciled.status,
                Err(e) => {
                    tracing::error!(task = %format!("{atespace}/{name}"), error = %e.status.message(), "reconciling new task");
                    self.record_failure(&mut task, atespace, name, &e).await;
                    return Err(substrate_error("provisioning task on substrate", &e.status));
                }
            }
        }
        self.tasks
            .update(atespace, name, task.status.as_ref())
            .await
            .map_err(internal("updating task status"))?;

        // Report the actor as it is now, as GetTask would.
        self.tasks.apply_actor(&mut task).await;
        Ok(task)
    }

    /// Records a failed provisioning or resume: Pending when Substrate may
    /// still come back, Failed otherwise.
    async fn record_failure(
        &self,
        task: &mut Task,
        atespace: &str,
        name: &str,
        e: &crate::ReconcileError,
    ) {
        if let Some(st) = e.task.as_ref().and_then(|t| t.status.clone()) {
            task.status = Some(st);
        }
        let st = task.status.get_or_insert_with(TaskStatus::default);
        if is_transient(&e.status) {
            st.phase = phase::PENDING.into();
        } else if st.phase != phase::PENDING {
            st.phase = phase::FAILED.into();
        }
        if let Err(err) = self
            .tasks
            .update(atespace, name, task.status.as_ref())
            .await
        {
            tracing::warn!(error = %err, "recording task failure");
        }
    }

    pub async fn delete_task(
        &self,
        req: api::DeleteTaskRequest,
    ) -> Result<api::DeleteTaskResponse, Status> {
        let atespace = atespace_or_default(&req.atespace);
        let name = req.name;
        self.locked("task", &atespace, &name, async {
            let mut task = self
                .tasks
                .get(&atespace, &name)
                .await
                .map_err(read_error("task", &name, &atespace))?;
            // Mark the task as terminating while actor cleanup is in progress.
            task.status.get_or_insert_with(Default::default).phase = phase::TERMINATING.into();
            self.tasks
                .update(&atespace, &name, task.status.as_ref())
                .await
                .map_err(internal("updating task status"))?;
            self.tasks
                .delete(self.reconciler.as_deref(), &atespace, &name)
                .await?;
            Ok(api::DeleteTaskResponse {})
        })
        .await
    }

    pub async fn suspend_task(&self, req: api::SuspendTaskRequest) -> Result<Task, Status> {
        let atespace = atespace_or_default(&req.atespace);
        let name = req.name;
        self.locked("task", &atespace, &name, async {
            let mut task = self
                .tasks
                .get(&atespace, &name)
                .await
                .map_err(read_error("task", &name, &atespace))?;
            task.status.get_or_insert_with(Default::default).phase = phase::SUSPENDED.into();
            if let Some(r) = &self.reconciler {
                let ws = self.fetch_workspace(&atespace, &task).await;
                // A failed suspension leaves the task as it was.
                let reconciled = r
                    .reconcile(task.clone(), ws)
                    .await
                    .map_err(|e| substrate_error("suspending task on substrate", &e.status))?;
                task.status = reconciled.status;
            }
            self.tasks
                .update(&atespace, &name, task.status.as_ref())
                .await
                .map_err(internal("updating task status"))?;
            self.tasks.apply_actor(&mut task).await;
            Ok(task)
        })
        .await
    }

    pub async fn resume_task(&self, req: api::ResumeTaskRequest) -> Result<Task, Status> {
        let atespace = atespace_or_default(&req.atespace);
        let name = req.name;
        self.locked("task", &atespace, &name, async {
            let mut task = self
                .tasks
                .get(&atespace, &name)
                .await
                .map_err(read_error("task", &name, &atespace))?;
            task.status.get_or_insert_with(Default::default).phase = phase::RUNNING.into();
            if let Some(r) = &self.reconciler {
                let ws = self.fetch_workspace(&atespace, &task).await;
                match r.reconcile(task.clone(), ws).await {
                    Ok(reconciled) => task.status = reconciled.status,
                    Err(e) => {
                        self.record_failure(&mut task, &atespace, &name, &e).await;
                        return Err(substrate_error("resuming task on substrate", &e.status));
                    }
                }
            }
            self.tasks
                .update(&atespace, &name, task.status.as_ref())
                .await
                .map_err(internal("updating task status"))?;
            self.tasks.apply_actor(&mut task).await;
            Ok(task)
        })
        .await
    }

    /// Loads the Workspace the task binds, or `None` when it binds none or the
    /// Workspace does not exist.
    async fn fetch_workspace(&self, atespace: &str, task: &Task) -> Option<Workspace> {
        let name = task.spec.as_ref()?.workspace.as_ref()?.name.as_str();
        if name.is_empty() {
            return None;
        }
        self.resources.get_workspace(atespace, name).await.ok()
    }

    // --- Workspaces ---

    pub async fn get_workspace(&self, req: api::GetWorkspaceRequest) -> Result<Workspace, Status> {
        let atespace = atespace_or_default(&req.atespace);
        self.resources
            .get_workspace(&atespace, &req.name)
            .await
            .map_err(read_error("workspace", &req.name, &atespace))
    }

    pub async fn list_workspaces(
        &self,
        req: api::ListWorkspacesRequest,
    ) -> Result<api::ListWorkspacesResponse, Status> {
        let workspaces = self
            .resources
            .list_workspaces(&req.atespace)
            .await
            .map_err(internal("listing workspaces"))?;
        Ok(api::ListWorkspacesResponse { workspaces })
    }

    pub async fn update_workspace(
        &self,
        req: api::UpdateWorkspaceRequest,
    ) -> Result<Workspace, Status> {
        let mut ws = req
            .workspace
            .ok_or_else(|| Status::invalid_argument("workspace required"))?;
        rax_api::validate_workspace(&ws).map_err(|e| Status::invalid_argument(e.to_string()))?;
        let res = self.resources.clone();
        let meta = default_metadata(ws.metadata.take(), |a, n| async move {
            res.get_workspace(&a, &n)
                .await
                .ok()
                .and_then(|w| w.metadata)
        })
        .await;
        let (atespace, name) = (meta.atespace.clone(), meta.name.clone());
        ws.metadata = Some(meta);
        self.locked("workspace", &atespace, &name, async {
            self.resources
                .save_workspace(&mut ws)
                .await
                .map_err(internal("saving workspace"))?;
            Ok(ws)
        })
        .await
    }

    pub async fn delete_workspace(
        &self,
        req: api::DeleteWorkspaceRequest,
    ) -> Result<api::DeleteWorkspaceResponse, Status> {
        let atespace = atespace_or_default(&req.atespace);
        self.locked("workspace", &atespace, &req.name, async {
            self.resources
                .delete_workspace(&atespace, &req.name)
                .await
                .map_err(internal("deleting workspace"))?;
            Ok(api::DeleteWorkspaceResponse {})
        })
        .await
    }

    // --- Models ---

    pub async fn get_model(&self, req: api::GetModelRequest) -> Result<api::Model, Status> {
        let atespace = atespace_or_default(&req.atespace);
        self.resources
            .get_model(&atespace, &req.name)
            .await
            .map_err(read_error("model", &req.name, &atespace))
    }

    pub async fn list_models(
        &self,
        req: api::ListModelsRequest,
    ) -> Result<api::ListModelsResponse, Status> {
        let models = self
            .resources
            .list_models(&req.atespace)
            .await
            .map_err(internal("listing models"))?;
        Ok(api::ListModelsResponse { models })
    }

    pub async fn update_model(&self, req: api::UpdateModelRequest) -> Result<api::Model, Status> {
        let mut model = req
            .model
            .ok_or_else(|| Status::invalid_argument("model required"))?;
        rax_api::validate_model(&model).map_err(|e| Status::invalid_argument(e.to_string()))?;
        let res = self.resources.clone();
        let meta = default_metadata(model.metadata.take(), |a, n| async move {
            res.get_model(&a, &n).await.ok().and_then(|m| m.metadata)
        })
        .await;
        let (atespace, name) = (meta.atespace.clone(), meta.name.clone());
        model.metadata = Some(meta);
        self.locked("model", &atespace, &name, async {
            self.resources
                .save_model(&mut model)
                .await
                .map_err(internal("saving model"))?;
            Ok(model)
        })
        .await
    }

    pub async fn delete_model(
        &self,
        req: api::DeleteModelRequest,
    ) -> Result<api::DeleteModelResponse, Status> {
        let atespace = atespace_or_default(&req.atespace);
        self.locked("model", &atespace, &req.name, async {
            self.resources
                .delete_model(&atespace, &req.name)
                .await
                .map_err(internal("deleting model"))?;
            Ok(api::DeleteModelResponse {})
        })
        .await
    }
}

/// Maps a Substrate error to the status the caller sees: Unavailable when it
/// is worth retrying, Internal otherwise.
fn substrate_error(context: &str, e: &Status) -> Status {
    let msg = format!("{context}: {}", e.message());
    if is_transient(e) {
        Status::unavailable(msg)
    } else {
        Status::internal(msg)
    }
}

/// Defaults the atespace and keeps the creation time of the resource being
/// replaced, or stamps a new one.
async fn default_metadata<F, Fut>(meta: Option<ObjectMeta>, existing: F) -> ObjectMeta
where
    F: FnOnce(String, String) -> Fut,
    Fut: Future<Output = Option<ObjectMeta>>,
{
    let mut meta = meta.unwrap_or_default();
    if meta.atespace.is_empty() {
        meta.atespace = DEFAULT_ATESPACE.into();
    }
    if meta.create_time.is_none() {
        let prev = existing(meta.atespace.clone(), meta.name.clone()).await;
        meta.create_time = Some(prev.and_then(|p| p.create_time).unwrap_or_else(now));
    }
    meta
}

#[tonic::async_trait]
impl Ax for AxService {
    async fn get_task(&self, r: Request<api::GetTaskRequest>) -> Result<Response<Task>, Status> {
        self.get_task(r.into_inner()).await.map(Response::new)
    }
    async fn list_tasks(
        &self,
        r: Request<api::ListTasksRequest>,
    ) -> Result<Response<api::ListTasksResponse>, Status> {
        self.list_tasks(r.into_inner()).await.map(Response::new)
    }
    async fn create_task(
        &self,
        r: Request<api::CreateTaskRequest>,
    ) -> Result<Response<Task>, Status> {
        self.create_task(r.into_inner()).await.map(Response::new)
    }
    async fn delete_task(
        &self,
        r: Request<api::DeleteTaskRequest>,
    ) -> Result<Response<api::DeleteTaskResponse>, Status> {
        self.delete_task(r.into_inner()).await.map(Response::new)
    }
    async fn suspend_task(
        &self,
        r: Request<api::SuspendTaskRequest>,
    ) -> Result<Response<Task>, Status> {
        self.suspend_task(r.into_inner()).await.map(Response::new)
    }
    async fn resume_task(
        &self,
        r: Request<api::ResumeTaskRequest>,
    ) -> Result<Response<Task>, Status> {
        self.resume_task(r.into_inner()).await.map(Response::new)
    }
    async fn get_workspace(
        &self,
        r: Request<api::GetWorkspaceRequest>,
    ) -> Result<Response<Workspace>, Status> {
        self.get_workspace(r.into_inner()).await.map(Response::new)
    }
    async fn list_workspaces(
        &self,
        r: Request<api::ListWorkspacesRequest>,
    ) -> Result<Response<api::ListWorkspacesResponse>, Status> {
        self.list_workspaces(r.into_inner())
            .await
            .map(Response::new)
    }
    async fn update_workspace(
        &self,
        r: Request<api::UpdateWorkspaceRequest>,
    ) -> Result<Response<Workspace>, Status> {
        self.update_workspace(r.into_inner())
            .await
            .map(Response::new)
    }
    async fn delete_workspace(
        &self,
        r: Request<api::DeleteWorkspaceRequest>,
    ) -> Result<Response<api::DeleteWorkspaceResponse>, Status> {
        self.delete_workspace(r.into_inner())
            .await
            .map(Response::new)
    }
    async fn get_model(
        &self,
        r: Request<api::GetModelRequest>,
    ) -> Result<Response<api::Model>, Status> {
        self.get_model(r.into_inner()).await.map(Response::new)
    }
    async fn list_models(
        &self,
        r: Request<api::ListModelsRequest>,
    ) -> Result<Response<api::ListModelsResponse>, Status> {
        self.list_models(r.into_inner()).await.map(Response::new)
    }
    async fn update_model(
        &self,
        r: Request<api::UpdateModelRequest>,
    ) -> Result<Response<api::Model>, Status> {
        self.update_model(r.into_inner()).await.map(Response::new)
    }
    async fn delete_model(
        &self,
        r: Request<api::DeleteModelRequest>,
    ) -> Result<Response<api::DeleteModelResponse>, Status> {
        self.delete_model(r.into_inner()).await.map(Response::new)
    }
}
