//! An in-memory Substrate control server for tests.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Endpoint, Server};
use tonic::{Request, Response, Status};

use crate::ateapi::control_server::{Control, ControlServer};
use crate::ateapi::*;
use crate::client::TokenInterceptor;
use crate::{Client, RetryPolicy};

/// The worker IP actors get when they resume, unless a test sets another.
pub const DEFAULT_WORKER_IP: &str = "10.244.1.42";

/// The fake's state. Tests read and change it between calls.
#[derive(Debug)]
pub struct State {
    pub atespaces: BTreeSet<String>,
    /// Templates by "atespace/name".
    pub templates: BTreeMap<String, ActorTemplate>,
    /// Every template created, in order.
    pub created_templates: Vec<ActorTemplate>,
    /// Actors by "atespace/name".
    pub actors: BTreeMap<String, Actor>,
    /// Every call, as "Method atespace/name".
    pub calls: Vec<String>,
    pub worker_ip: String,
    /// How many GetActor or CreateActor calls a deleted actor stays visible
    /// as DELETING.
    /// `usize::MAX` means it never goes away.
    pub deleting_polls: usize,
    /// Actors being deleted, with the number of polls left.
    pub deleting: HashMap<String, usize>,
    /// Errors to return, per method name, before doing the call.
    pub errors: HashMap<&'static str, VecDeque<Status>>,
    /// The page size for ListActors; 0 means everything at once.
    pub list_page_size: usize,
    /// The authorization header on the latest call.
    pub last_authorization: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            atespaces: BTreeSet::new(),
            templates: BTreeMap::new(),
            created_templates: Vec::new(),
            actors: BTreeMap::new(),
            calls: Vec::new(),
            worker_ip: DEFAULT_WORKER_IP.to_owned(),
            deleting_polls: 0,
            deleting: HashMap::new(),
            errors: HashMap::new(),
            list_page_size: 0,
            last_authorization: None,
        }
    }
}

impl State {
    /// Queues errors for the next calls of `method`, such as "CreateActor".
    pub fn fail(&mut self, method: &'static str, errors: impl IntoIterator<Item = Status>) {
        self.errors.entry(method).or_default().extend(errors);
    }

    /// The calls of one method, as "atespace/name".
    pub fn calls_of(&self, method: &str) -> Vec<String> {
        let prefix = format!("{method} ");
        self.calls
            .iter()
            .filter_map(|c| c.strip_prefix(&prefix).map(str::to_owned))
            .collect()
    }

    /// Adds an actor in the given state, as if it had been created earlier.
    pub fn put_actor(&mut self, atespace: &str, name: &str, state: ActorState) {
        self.actors
            .insert(key(atespace, name), actor(atespace, name, state, None));
    }

    /// Counts a look at an actor being deleted, removing it once no looks
    /// are left.
    fn advance_deletion(&mut self, k: &str) {
        match self.deleting.get(k).copied() {
            Some(0) => {
                self.deleting.remove(k);
                self.actors.remove(k);
            }
            Some(usize::MAX) | None => {}
            Some(left) => {
                self.deleting.insert(k.to_owned(), left - 1);
            }
        }
    }

    pub fn actor_state(&self, atespace: &str, name: &str) -> Option<ActorState> {
        self.actors
            .get(&key(atespace, name))
            .and_then(|a| a.status.as_ref())
            .and_then(|s| ActorState::try_from(s.state).ok())
    }
}

fn key(atespace: &str, name: &str) -> String {
    format!("{atespace}/{name}")
}

fn ref_key(r: &Option<ObjectRef>) -> String {
    r.as_ref()
        .map(|r| key(&r.atespace, &r.name))
        .unwrap_or_default()
}

fn meta_key(m: &Option<ResourceMetadata>) -> String {
    m.as_ref()
        .map(|m| key(&m.atespace, &m.name))
        .unwrap_or_default()
}

fn actor(atespace: &str, name: &str, state: ActorState, worker_ip: Option<&str>) -> Actor {
    Actor {
        metadata: Some(ResourceMetadata {
            atespace: atespace.to_owned(),
            name: name.to_owned(),
            ..Default::default()
        }),
        status: Some(ActorStatus {
            state: state as i32,
            worker_assignment: worker_ip.map(|ip| WorkerAssignment {
                worker_pod: "worker-pod-1".to_owned(),
                worker_pod_ips: vec![ip.to_owned()],
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn set_state(a: &mut Actor, state: ActorState, worker_ip: Option<&str>) {
    let (atespace, name) = a
        .metadata
        .as_ref()
        .map(|m| (m.atespace.clone(), m.name.clone()))
        .unwrap_or_default();
    let template = a.actor_template.take();
    *a = actor(&atespace, &name, state, worker_ip);
    a.actor_template = template;
}

fn not_found(what: &str, k: &str) -> Status {
    Status::not_found(format!("{what} {k:?} not found"))
}

/// A fake Substrate control server holding its state in memory.
#[derive(Clone, Default)]
pub struct FakeSubstrate {
    pub state: Arc<Mutex<State>>,
}

impl FakeSubstrate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves the fake on a local port and returns a client for it, with
    /// fast retries.
    pub async fn start(&self) -> Client {
        self.start_with_auth(TokenInterceptor::default()).await
    }

    pub async fn start_with_auth(&self, auth: TokenInterceptor) -> Client {
        let addr = self.serve().await;
        let channel = Endpoint::from_shared(format!("http://{addr}"))
            .expect("endpoint")
            .connect_lazy();
        Client::from_channel(channel, auth).with_retry_policy(RetryPolicy::fast())
    }

    /// Serves the fake on a local port and returns its address.
    pub async fn serve(&self) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let svc = ControlServer::new(self.clone());
        tokio::spawn(async move {
            Server::builder()
                .add_service(svc)
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .expect("fake substrate");
        });
        addr
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Records the call and returns the next queued error for it, if any.
    fn begin<T>(
        &self,
        method: &'static str,
        k: &str,
        req: &Request<T>,
    ) -> Result<std::sync::MutexGuard<'_, State>, Status> {
        let mut s = self.state();
        s.calls.push(format!("{method} {k}"));
        s.last_authorization = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if let Some(err) = s.errors.get_mut(method).and_then(VecDeque::pop_front) {
            return Err(err);
        }
        Ok(s)
    }
}

#[tonic::async_trait]
impl Control for FakeSubstrate {
    async fn create_atespace(
        &self,
        req: Request<CreateAtespaceRequest>,
    ) -> Result<Response<Atespace>, Status> {
        let name = req
            .get_ref()
            .atespace
            .as_ref()
            .and_then(|a| a.metadata.as_ref())
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let mut s = self.begin("CreateAtespace", &name, &req)?;
        if !s.atespaces.insert(name.clone()) {
            return Err(Status::already_exists(format!(
                "atespace {name:?} already exists"
            )));
        }
        Ok(Response::new(req.into_inner().atespace.unwrap_or_default()))
    }

    async fn get_actor_template(
        &self,
        req: Request<GetActorTemplateRequest>,
    ) -> Result<Response<ActorTemplate>, Status> {
        let k = ref_key(&req.get_ref().actor_template);
        let s = self.begin("GetActorTemplate", &k, &req)?;
        s.templates
            .get(&k)
            .cloned()
            .map(Response::new)
            .ok_or_else(|| not_found("template", &k))
    }

    async fn create_actor_template(
        &self,
        req: Request<CreateActorTemplateRequest>,
    ) -> Result<Response<ActorTemplate>, Status> {
        let tmpl = req.get_ref().actor_template.clone().unwrap_or_default();
        let k = meta_key(&tmpl.metadata);
        let mut s = self.begin("CreateActorTemplate", &k, &req)?;
        if s.templates.contains_key(&k) {
            return Err(Status::already_exists(format!(
                "template {k:?} already exists"
            )));
        }
        s.templates.insert(k, tmpl.clone());
        s.created_templates.push(tmpl.clone());
        Ok(Response::new(tmpl))
    }

    async fn list_actor_templates(
        &self,
        req: Request<ListActorTemplatesRequest>,
    ) -> Result<Response<ListActorTemplatesResponse>, Status> {
        let atespace = req.get_ref().atespace.clone();
        let s = self.begin("ListActorTemplates", &atespace, &req)?;
        let actor_templates = s
            .templates
            .values()
            .filter(|t| t.metadata.as_ref().is_some_and(|m| m.atespace == atespace))
            .cloned()
            .collect();
        Ok(Response::new(ListActorTemplatesResponse {
            actor_templates,
            next_page_token: String::new(),
        }))
    }

    async fn delete_actor_template(
        &self,
        req: Request<DeleteActorTemplateRequest>,
    ) -> Result<Response<ActorTemplate>, Status> {
        let k = ref_key(&req.get_ref().actor_template);
        let mut s = self.begin("DeleteActorTemplate", &k, &req)?;
        s.templates
            .remove(&k)
            .map(Response::new)
            .ok_or_else(|| not_found("template", &k))
    }

    async fn create_actor(
        &self,
        req: Request<CreateActorRequest>,
    ) -> Result<Response<Actor>, Status> {
        let mut a = req.get_ref().actor.clone().unwrap_or_default();
        let k = meta_key(&a.metadata);
        let mut s = self.begin("CreateActor", &k, &req)?;
        s.advance_deletion(&k);
        if s.actors.contains_key(&k) {
            return Err(Status::already_exists(format!(
                "actor {k:?} already exists"
            )));
        }
        a.status = Some(ActorStatus {
            state: ActorState::Suspended as i32,
            ..Default::default()
        });
        s.actors.insert(k, a.clone());
        Ok(Response::new(a))
    }

    async fn get_actor(&self, req: Request<GetActorRequest>) -> Result<Response<Actor>, Status> {
        let k = ref_key(&req.get_ref().actor);
        let mut s = self.begin("GetActor", &k, &req)?;
        s.advance_deletion(&k);
        s.actors
            .get(&k)
            .cloned()
            .map(Response::new)
            .ok_or_else(|| not_found("actor", &k))
    }

    async fn list_actors(
        &self,
        req: Request<ListActorsRequest>,
    ) -> Result<Response<ListActorsResponse>, Status> {
        let atespace = req.get_ref().atespace.clone();
        let token = req
            .get_ref()
            .options
            .as_ref()
            .map(|o| o.page_token.clone())
            .unwrap_or_default();
        let s = self.begin("ListActors", &atespace, &req)?;
        let all: Vec<Actor> = s
            .actors
            .values()
            .filter(|a| {
                atespace.is_empty() || a.metadata.as_ref().is_some_and(|m| m.atespace == atespace)
            })
            .cloned()
            .collect();
        let start: usize = token.parse().unwrap_or(0);
        let size = if s.list_page_size == 0 {
            all.len()
        } else {
            s.list_page_size
        };
        let end = (start + size).min(all.len());
        let next_page_token = if end < all.len() {
            end.to_string()
        } else {
            String::new()
        };
        Ok(Response::new(ListActorsResponse {
            actors: all[start..end].to_vec(),
            next_page_token,
        }))
    }

    async fn resume_actor(
        &self,
        req: Request<ResumeActorRequest>,
    ) -> Result<Response<ResumeActorResponse>, Status> {
        let k = ref_key(&req.get_ref().actor);
        let mut s = self.begin("ResumeActor", &k, &req)?;
        let ip = s.worker_ip.clone();
        let a = s.actors.get_mut(&k).ok_or_else(|| not_found("actor", &k))?;
        set_state(a, ActorState::Running, Some(&ip));
        Ok(Response::new(ResumeActorResponse {
            actor: Some(a.clone()),
            resumed: true,
        }))
    }

    async fn suspend_actor(
        &self,
        req: Request<SuspendActorRequest>,
    ) -> Result<Response<SuspendActorResponse>, Status> {
        let k = ref_key(&req.get_ref().actor);
        let mut s = self.begin("SuspendActor", &k, &req)?;
        let a = s.actors.get_mut(&k).ok_or_else(|| not_found("actor", &k))?;
        set_state(a, ActorState::Suspended, None);
        Ok(Response::new(SuspendActorResponse {
            actor: Some(a.clone()),
        }))
    }

    async fn revert_actor(
        &self,
        req: Request<RevertActorRequest>,
    ) -> Result<Response<RevertActorResponse>, Status> {
        let k = ref_key(&req.get_ref().actor);
        let mut s = self.begin("RevertActor", &k, &req)?;
        let a = s.actors.get_mut(&k).ok_or_else(|| not_found("actor", &k))?;
        set_state(a, ActorState::Suspended, None);
        Ok(Response::new(RevertActorResponse {
            actor: Some(a.clone()),
        }))
    }

    async fn delete_actor(
        &self,
        req: Request<DeleteActorRequest>,
    ) -> Result<Response<Actor>, Status> {
        let k = ref_key(&req.get_ref().actor);
        let mut s = self.begin("DeleteActor", &k, &req)?;
        let polls = s.deleting_polls;
        let Some(a) = s.actors.get_mut(&k) else {
            return Err(not_found("actor", &k));
        };
        if polls == 0 {
            let a = s.actors.remove(&k).unwrap_or_default();
            return Ok(Response::new(a));
        }
        set_state(a, ActorState::Deleting, None);
        let a = a.clone();
        s.deleting.insert(k, polls);
        Ok(Response::new(a))
    }
}
