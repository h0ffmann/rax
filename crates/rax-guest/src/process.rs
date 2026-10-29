//! The process service: commands started in the background, addressed by an
//! id, with their output spooled to disk so it can be streamed live or
//! replayed from an offset.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, watch};
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::proto::{
    self, GetProcessRequest, ProcessOutput, SignalProcessRequest, StartProcessRequest,
    StreamProcessOutputRequest, WriteProcessInputRequest, WriteProcessInputResponse,
    process_output::Output, process_service_server::ProcessService,
};
use crate::signal::{from_syscall_signal, to_syscall_signal};

/// Where process output is spooled when neither the caller nor `$LOG_DIR`
/// says otherwise.
pub const DEFAULT_LOG_DIR: &str = "/var/log/ate-jobs";
/// How many processes may run at once by default.
pub const DEFAULT_MAX_CONCURRENT_PROCESSES: usize = 10;
/// The default cap on each of a process's stdout and stderr spools (10 MB).
pub const DEFAULT_MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
/// How long a process may run before its group is killed, by default.
pub const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_secs(3600);
/// How long a finished process and its spools are kept, by default.
pub const DEFAULT_RETENTION_PERIOD: Duration = Duration::from_secs(3600);
/// How many finished processes are kept, by default.
pub const DEFAULT_MAX_RETAINED_PROCESSES: usize = 100;

const PRUNE_INTERVAL: Duration = Duration::from_secs(120);
const COPY_BUFFER: usize = 32 * 1024;

/// Why a tracker operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Exited,
    NoStdin,
    StdinClosed,
    TooManyProcesses(usize),
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => f.write_str("process not found"),
            Error::Exited => f.write_str("process has exited"),
            Error::NoStdin => f.write_str("process was started without stdin"),
            Error::StdinClosed => f.write_str("stdin is closed"),
            Error::TooManyProcesses(n) => {
                write!(f, "maximum concurrent processes limit reached ({n})")
            }
            Error::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for Error {}

/// Limits and locations for a [`Tracker`]. Zero limits mean unlimited.
#[derive(Debug, Clone)]
pub struct TrackerConfig {
    pub log_dir: PathBuf,
    /// The working directory of processes that don't ask for one.
    pub workspace: Option<PathBuf>,
    /// Added to the guest's own environment for every process, under what
    /// the process asks for.
    pub env: HashMap<String, String>,
    pub max_concurrent_processes: usize,
    pub max_log_bytes: u64,
    /// Applies to processes started without a timeout of their own.
    pub default_process_timeout: Duration,
    pub retention_period: Duration,
    pub max_retained_processes: usize,
}

impl TrackerConfig {
    /// The default limits, spooling to `log_dir` or, when it is `None`, to
    /// `$LOG_DIR`, [`DEFAULT_LOG_DIR`] if it can be created, or `ate-jobs`
    /// under the temp directory.
    pub fn with_log_dir(log_dir: Option<PathBuf>) -> Self {
        Self {
            log_dir: log_dir.unwrap_or_else(default_log_dir),
            workspace: None,
            env: HashMap::new(),
            max_concurrent_processes: DEFAULT_MAX_CONCURRENT_PROCESSES,
            max_log_bytes: DEFAULT_MAX_LOG_BYTES,
            default_process_timeout: DEFAULT_PROCESS_TIMEOUT,
            retention_period: DEFAULT_RETENTION_PERIOD,
            max_retained_processes: DEFAULT_MAX_RETAINED_PROCESSES,
        }
    }
}

fn default_log_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LOG_DIR").filter(|d| !d.is_empty()) {
        return dir.into();
    }
    if std::fs::create_dir_all(DEFAULT_LOG_DIR).is_ok() {
        return DEFAULT_LOG_DIR.into();
    }
    std::env::temp_dir().join("ate-jobs")
}

/// What to start.
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    pub command: Vec<String>,
    /// Empty means the tracker's workspace.
    pub cwd: String,
    /// Added to the guest's own environment.
    pub env: HashMap<String, String>,
    /// Open a stdin pipe fed by [`Tracker::write_input`]. Otherwise stdin
    /// reads as empty.
    pub stdin: bool,
    /// Zero means the tracker's default.
    pub timeout: Duration,
}

#[derive(Debug, Clone, Copy)]
struct ExitInfo {
    code: i32,
    /// Nonzero when the process was killed by a signal.
    signal: i32,
    finished_at: SystemTime,
}

#[derive(Debug, Default)]
struct StdinState {
    pipe: Option<ChildStdin>,
    closed: bool,
}

/// One process the tracker started.
#[derive(Debug)]
pub struct ProcessState {
    pub process_id: String,
    pub command: Vec<String>,
    pub pid: i32,
    pub started_at: SystemTime,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
    exit: Mutex<Option<ExitInfo>>,
    has_stdin: bool,
    stdin: tokio::sync::Mutex<StdinState>,
    /// Bumped on every spool write and once more on exit.
    changes: watch::Sender<u64>,
}

impl ProcessState {
    pub fn exited(&self) -> bool {
        self.exit.lock().unwrap().is_some()
    }

    /// A receiver that wakes on new output and on exit.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn notify(&self) {
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    fn finished_at(&self) -> Option<SystemTime> {
        self.exit.lock().unwrap().map(|e| e.finished_at)
    }

    pub fn to_proto(&self) -> proto::Process {
        let exit = *self.exit.lock().unwrap();
        let mut p = proto::Process {
            process_id: self.process_id.clone(),
            command: self.command.clone(),
            pid: self.pid,
            state: proto::ProcessState::Running as i32,
            exit_code: 0,
            started_at: Some(self.started_at.into()),
            finished_at: None,
        };
        if let Some(exit) = exit {
            p.state = proto::ProcessState::Exited as i32;
            p.exit_code = if exit.signal != 0 {
                128 + from_syscall_signal(exit.signal) as i32
            } else {
                exit.code
            };
            p.finished_at = Some(exit.finished_at.into());
        }
        p
    }
}

#[derive(Debug, Default)]
struct Processes {
    by_id: HashMap<String, Arc<ProcessState>>,
    active: usize,
}

#[derive(Debug)]
struct Inner {
    config: TrackerConfig,
    processes: Mutex<Processes>,
}

/// Starts processes and keeps their state and spooled output. Clones share
/// the same processes. Finished processes are pruned in the background
/// while any clone is alive.
#[derive(Debug, Clone)]
pub struct Tracker {
    inner: Arc<Inner>,
}

impl Tracker {
    /// Creates the log directory and starts pruning. Must be called inside a
    /// Tokio runtime.
    pub fn new(mut config: TrackerConfig) -> std::io::Result<Self> {
        if config.log_dir.as_os_str().is_empty() {
            config.log_dir = std::env::temp_dir().join("ate-jobs");
        }
        std::fs::create_dir_all(&config.log_dir)
            .map_err(|e| std::io::Error::new(e.kind(), format!("creating log directory: {e}")))?;
        let inner = Arc::new(Inner {
            config,
            processes: Mutex::default(),
        });
        tokio::spawn(prune_loop(Arc::downgrade(&inner)));
        Ok(Self { inner })
    }

    fn config(&self) -> &TrackerConfig {
        &self.inner.config
    }

    fn generate_id(&self) -> String {
        loop {
            let id = format!("proc-{}", hex(&rand::random::<[u8; 12]>()));
            if !self.inner.processes.lock().unwrap().by_id.contains_key(&id) {
                return id;
            }
        }
    }

    fn decrement_active(&self) {
        let mut p = self.inner.processes.lock().unwrap();
        p.active = p.active.saturating_sub(1);
    }

    /// Starts a process in its own process group and returns right away.
    pub fn start(&self, opts: StartOptions) -> Result<Arc<ProcessState>, Error> {
        let Some((program, args)) = opts.command.split_first() else {
            return Err(Error::Other("command cannot be empty".into()));
        };
        {
            let mut p = self.inner.processes.lock().unwrap();
            let max = self.config().max_concurrent_processes;
            if max > 0 && p.active >= max {
                return Err(Error::TooManyProcesses(max));
            }
            p.active += 1;
        }
        let started = self.spawn(program, args, &opts);
        if started.is_err() {
            self.decrement_active();
        }
        started
    }

    fn spawn(
        &self,
        program: &str,
        args: &[String],
        opts: &StartOptions,
    ) -> Result<Arc<ProcessState>, Error> {
        let id = self.generate_id();
        let stdout_path = self.config().log_dir.join(format!("{id}.stdout"));
        let stderr_path = self.config().log_dir.join(format!("{id}.stderr"));
        let stdout_file = std::fs::File::create(&stdout_path)
            .map_err(|e| Error::Other(format!("creating stdout log: {e}")))?;
        let stderr_file = std::fs::File::create(&stderr_path)
            .map_err(|e| Error::Other(format!("creating stderr log: {e}")))?;

        let mut cmd = Command::new(program);
        cmd.args(args)
            .envs(&self.config().env)
            .envs(&opts.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if opts.stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            // Its own group, so signals reach the whole tree.
            .process_group(0);
        if !opts.cwd.is_empty() {
            cmd.current_dir(&opts.cwd);
        } else if let Some(ws) = &self.config().workspace {
            cmd.current_dir(ws);
        }
        let started_at = SystemTime::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Other(format!("starting process: {e}")))?;
        let pid = child.id().map_or(0, |p| p as i32);

        let state = Arc::new(ProcessState {
            process_id: id.clone(),
            command: opts.command.clone(),
            pid,
            started_at,
            stdout_path,
            stderr_path,
            exit: Mutex::new(None),
            has_stdin: opts.stdin,
            stdin: tokio::sync::Mutex::new(StdinState {
                pipe: child.stdin.take(),
                closed: false,
            }),
            changes: watch::channel(0).0,
        });

        let limit = self.config().max_log_bytes;
        let copiers = [
            child
                .stdout
                .take()
                .map(|out| tokio::spawn(copy_capped(out, stdout_file, limit, state.clone()))),
            child
                .stderr
                .take()
                .map(|err| tokio::spawn(copy_capped(err, stderr_file, limit, state.clone()))),
        ];

        let timeout = if opts.timeout.is_zero() {
            self.config().default_process_timeout
        } else {
            opts.timeout
        };
        let timer = (!timeout.is_zero()).then(|| {
            let tracker = self.clone();
            let id = id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(timeout).await;
                let _ = tracker.signal(&id, libc::SIGKILL);
            })
        });

        self.inner
            .processes
            .lock()
            .unwrap()
            .by_id
            .insert(id, state.clone());

        let tracker = self.clone();
        let reaped = state.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            // The spools are complete once both copiers have drained the
            // pipes.
            for copier in copiers.into_iter().flatten() {
                let _ = copier.await;
            }
            let (code, signal) = match status {
                Ok(s) => match s.signal() {
                    Some(sig) => (0, sig),
                    None => (s.code().unwrap_or(-1), 0),
                },
                Err(_) => (-1, 0),
            };
            if let Some(timer) = timer {
                timer.abort();
            }
            *reaped.exit.lock().unwrap() = Some(ExitInfo {
                code,
                signal,
                finished_at: SystemTime::now(),
            });
            {
                let mut stdin = reaped.stdin.lock().await;
                stdin.pipe = None;
                stdin.closed = true;
            }
            reaped.notify();
            tracker.decrement_active();
        });
        Ok(state)
    }

    pub fn get(&self, id: &str) -> Result<Arc<ProcessState>, Error> {
        self.inner
            .processes
            .lock()
            .unwrap()
            .by_id
            .get(id)
            .cloned()
            .ok_or(Error::NotFound)
    }

    /// Delivers `sig` to the process's group.
    pub fn signal(&self, id: &str, sig: libc::c_int) -> Result<(), Error> {
        let state = self.get(id)?;
        if state.exited() {
            return Err(Error::Exited);
        }
        // SAFETY: kill has no memory-safety preconditions.
        if unsafe { libc::kill(-state.pid, sig) } != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                return Err(Error::Exited);
            }
            return Err(Error::Other(format!("sending signal {sig}: {err}")));
        }
        Ok(())
    }

    /// Writes to the process's stdin and returns how many bytes were written.
    pub async fn write_input(&self, id: &str, data: &[u8]) -> Result<usize, Error> {
        let state = self.get(id)?;
        if !state.has_stdin {
            return Err(Error::NoStdin);
        }
        let mut stdin = state.stdin.lock().await;
        let closed = stdin.closed;
        let pipe = match (&mut stdin.pipe, closed) {
            (Some(pipe), false) => pipe,
            _ if state.exited() => return Err(Error::Exited),
            _ => return Err(Error::StdinClosed),
        };
        match pipe.write_all(data).await {
            Ok(()) => Ok(data.len()),
            Err(e) if state.exited() || e.kind() == std::io::ErrorKind::BrokenPipe => {
                Err(Error::Exited)
            }
            Err(e) => Err(Error::Other(format!("writing stdin: {e}"))),
        }
    }

    /// Closes the process's stdin. Closing it again is not an error.
    pub async fn close_input(&self, id: &str) -> Result<(), Error> {
        let state = self.get(id)?;
        if !state.has_stdin {
            return Err(Error::NoStdin);
        }
        let mut stdin = state.stdin.lock().await;
        if stdin.closed {
            return Ok(());
        }
        stdin.closed = true;
        if let Some(mut pipe) = stdin.pipe.take() {
            pipe.shutdown()
                .await
                .map_err(|e| Error::Other(format!("closing stdin: {e}")))?;
        }
        Ok(())
    }

    /// Forgets finished processes past the retention period, and the oldest
    /// ones beyond the retention cap, removing their spools.
    pub fn prune_expired(&self) {
        prune(&self.inner);
    }
}

async fn prune_loop(inner: Weak<Inner>) {
    let mut ticker = tokio::time::interval(PRUNE_INTERVAL);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let Some(inner) = inner.upgrade() else { return };
        prune(&inner);
    }
}

fn prune(inner: &Inner) {
    let cfg = &inner.config;
    let mut p = inner.processes.lock().unwrap();
    let now = SystemTime::now();
    let mut exited = Vec::new();
    let mut expired = Vec::new();
    for state in p.by_id.values() {
        let Some(finished_at) = state.finished_at() else {
            continue;
        };
        let age = now.duration_since(finished_at).unwrap_or_default();
        if !cfg.retention_period.is_zero() && age > cfg.retention_period {
            expired.push(state.clone());
        } else {
            exited.push((finished_at, state.clone()));
        }
    }
    if cfg.max_retained_processes > 0 && exited.len() > cfg.max_retained_processes {
        exited.sort_by_key(|(t, _)| *t);
        let excess = exited.len() - cfg.max_retained_processes;
        expired.extend(exited.into_iter().take(excess).map(|(_, s)| s));
    }
    for state in expired {
        let _ = std::fs::remove_file(&state.stdout_path);
        let _ = std::fs::remove_file(&state.stderr_path);
        p.by_id.remove(&state.process_id);
    }
}

/// Copies a pipe into its spool, dropping what goes past `limit` (unless it
/// is zero) after one note saying so, and wakes streamers on every chunk.
async fn copy_capped(
    mut src: impl AsyncRead + Unpin,
    mut file: std::fs::File,
    limit: u64,
    state: Arc<ProcessState>,
) {
    let mut buf = vec![0; COPY_BUFFER];
    let mut written = 0u64;
    let mut warned = false;
    loop {
        let n = match src.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let chunk = &buf[..n];
        if limit == 0 {
            let _ = file.write_all(chunk);
        } else {
            let remaining = limit.saturating_sub(written);
            let take = chunk.len().min(remaining as usize);
            if take > 0 && file.write_all(&chunk[..take]).is_ok() {
                written += take as u64;
            }
            if take < chunk.len() && !warned {
                warned = true;
                let _ = write!(
                    file,
                    "\n\n[guest: maximum log limit of {} MB exceeded; remaining output truncated]\n",
                    limit / (1024 * 1024)
                );
            }
        }
        state.notify();
    }
    let _ = file.sync_all();
}

/// Reads a spool from `offset` to its current end. The returned offset never
/// moves backwards: an offset past the end means "skip everything so far".
pub fn read_spool(path: &Path, offset: i64) -> std::io::Result<(Vec<u8>, i64)> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), offset)),
        Err(e) => return Err(e),
    };
    let size = f.metadata()?.len() as i64;
    if offset >= size {
        return Ok((Vec::new(), offset));
    }
    f.seek(SeekFrom::Start(offset as u64))?;
    let mut buf = Vec::with_capacity((size - offset) as usize);
    f.take((size - offset) as u64).read_to_end(&mut buf)?;
    let next = offset + buf.len() as i64;
    Ok((buf, next))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `ProcessService` gRPC service over a [`Tracker`].
#[derive(Debug, Clone)]
pub struct Service {
    tracker: Tracker,
}

impl Service {
    pub fn new(tracker: Tracker) -> Self {
        Self { tracker }
    }

    fn lookup(&self, id: &str) -> Result<Arc<ProcessState>, Status> {
        if id.is_empty() {
            return Err(Status::invalid_argument("process_id cannot be empty"));
        }
        self.tracker.get(id).map_err(|e| rpc_error(e, id))
    }
}

fn rpc_error(err: Error, id: &str) -> Status {
    match err {
        Error::NotFound => Status::not_found(format!("process {id:?} not found")),
        Error::Exited => Status::failed_precondition(format!("process {id:?} has exited")),
        Error::NoStdin => {
            Status::failed_precondition(format!("process {id:?} was started without stdin"))
        }
        Error::StdinClosed => {
            Status::failed_precondition(format!("stdin of process {id:?} is closed"))
        }
        e @ Error::TooManyProcesses(_) => Status::resource_exhausted(format!(
            "{e}; wait for running processes to exit or signal them"
        )),
        Error::Other(msg) => Status::internal(msg),
    }
}

/// Converts a request timeout. `None` when it is invalid; zero when it is
/// unset or not positive.
fn timeout_of(d: Option<&prost_types::Duration>) -> Option<Duration> {
    let Some(d) = d else {
        return Some(Duration::ZERO);
    };
    const MAX_SECONDS: i64 = 315_576_000_000;
    let valid = d.seconds.abs() <= MAX_SECONDS
        && d.nanos.abs() < 1_000_000_000
        && !(d.seconds > 0 && d.nanos < 0 || d.seconds < 0 && d.nanos > 0);
    if !valid {
        return None;
    }
    if d.seconds < 0 || d.nanos < 0 {
        return Some(Duration::ZERO);
    }
    Some(Duration::new(d.seconds as u64, d.nanos as u32))
}

type OutputStream = Pin<Box<dyn Stream<Item = Result<ProcessOutput, Status>> + Send>>;

#[tonic::async_trait]
impl ProcessService for Service {
    async fn start_process(
        &self,
        req: Request<StartProcessRequest>,
    ) -> Result<Response<proto::Process>, Status> {
        let req = req.into_inner();
        if req.command.is_empty() {
            return Err(Status::invalid_argument("command cannot be empty"));
        }
        let timeout = timeout_of(req.timeout.as_ref())
            .ok_or_else(|| Status::invalid_argument("timeout is invalid"))?;
        let state = self
            .tracker
            .start(StartOptions {
                command: req.command,
                cwd: req.cwd,
                env: req.env,
                stdin: req.stdin,
                timeout,
            })
            .map_err(|e| rpc_error(e, ""))?;
        Ok(Response::new(state.to_proto()))
    }

    async fn get_process(
        &self,
        req: Request<GetProcessRequest>,
    ) -> Result<Response<proto::Process>, Status> {
        Ok(Response::new(
            self.lookup(&req.into_inner().process_id)?.to_proto(),
        ))
    }

    type StreamProcessOutputStream = OutputStream;

    async fn stream_process_output(
        &self,
        req: Request<StreamProcessOutputRequest>,
    ) -> Result<Response<OutputStream>, Status> {
        let req = req.into_inner();
        let state = self.lookup(&req.process_id)?;
        if req.stdout_offset < 0 || req.stderr_offset < 0 {
            return Err(Status::invalid_argument("offsets cannot be negative"));
        }
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(stream_output(
            state,
            tx,
            req.stdout_offset,
            req.stderr_offset,
            req.follow,
        ));
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn write_process_input(
        &self,
        req: Request<Streaming<WriteProcessInputRequest>>,
    ) -> Result<Response<WriteProcessInputResponse>, Status> {
        let mut stream = req.into_inner();
        let mut id = String::new();
        let mut total = 0i64;
        while let Some(msg) = stream.message().await? {
            if id.is_empty() {
                id = msg.process_id;
                if id.is_empty() {
                    return Err(Status::invalid_argument(
                        "process_id is required on the first message",
                    ));
                }
                self.tracker.get(&id).map_err(|e| rpc_error(e, &id))?;
            }
            if !msg.data.is_empty() {
                let n = self
                    .tracker
                    .write_input(&id, &msg.data)
                    .await
                    .map_err(|e| rpc_error(e, &id))?;
                total += n as i64;
            }
            if msg.close {
                self.tracker
                    .close_input(&id)
                    .await
                    .map_err(|e| rpc_error(e, &id))?;
            }
        }
        if id.is_empty() {
            return Err(Status::invalid_argument(
                "process_id is required on the first message",
            ));
        }
        Ok(Response::new(WriteProcessInputResponse {
            bytes_written: total,
        }))
    }

    async fn signal_process(
        &self,
        req: Request<SignalProcessRequest>,
    ) -> Result<Response<proto::Process>, Status> {
        let req = req.into_inner();
        let state = self.lookup(&req.process_id)?;
        let sig = proto::Signal::try_from(req.signal)
            .ok()
            .and_then(to_syscall_signal)
            .ok_or_else(|| {
                Status::invalid_argument(format!("unsupported signal {}", req.signal))
            })?;
        self.tracker
            .signal(&req.process_id, sig)
            .map_err(|e| rpc_error(e, &req.process_id))?;
        Ok(Response::new(state.to_proto()))
    }
}

/// Sends spooled output from the offsets on, then with `follow` keeps
/// sending until the process exits, ending with its final state.
async fn stream_output(
    state: Arc<ProcessState>,
    tx: mpsc::Sender<Result<ProcessOutput, Status>>,
    mut stdout: i64,
    mut stderr: i64,
    follow: bool,
) {
    let mut changes = state.subscribe();
    let flush = |stdout: &mut i64, stderr: &mut i64| -> Result<Vec<ProcessOutput>, Status> {
        let mut out = Vec::new();
        let (data, next) = read_spool(&state.stdout_path, *stdout)
            .map_err(|e| Status::internal(format!("reading stdout: {e}")))?;
        if !data.is_empty() {
            out.push(ProcessOutput {
                output: Some(Output::Stdout(data)),
            });
        }
        *stdout = next;
        let (data, next) = read_spool(&state.stderr_path, *stderr)
            .map_err(|e| Status::internal(format!("reading stderr: {e}")))?;
        if !data.is_empty() {
            out.push(ProcessOutput {
                output: Some(Output::Stderr(data)),
            });
        }
        *stderr = next;
        Ok(out)
    };
    loop {
        // Mark changes seen before reading so a write that lands after the
        // read still wakes us.
        changes.borrow_and_update();
        let mut batch = match flush(&mut stdout, &mut stderr) {
            Ok(b) => b,
            Err(e) => {
                let _ = tx.send(Err(e)).await;
                return;
            }
        };
        let exited = state.exited();
        if exited {
            // Output is complete once the process is marked exited; pick up
            // anything written after the read above.
            match flush(&mut stdout, &mut stderr) {
                Ok(more) => batch.extend(more),
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            }
            batch.push(ProcessOutput {
                output: Some(Output::Exit(state.to_proto())),
            });
        }
        for msg in batch {
            if tx.send(Ok(msg)).await.is_err() {
                return;
            }
        }
        if exited || !follow {
            return;
        }
        tokio::select! {
            _ = tx.closed() => return,
            r = changes.changed() => if r.is_err() { return },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker(dir: &Path) -> Tracker {
        let mut cfg = TrackerConfig::with_log_dir(Some(dir.to_owned()));
        cfg.max_concurrent_processes = 2;
        Tracker::new(cfg).unwrap()
    }

    async fn wait_exit(state: &ProcessState) -> proto::Process {
        let mut changes = state.subscribe();
        while !state.exited() {
            changes.changed().await.unwrap();
        }
        state.to_proto()
    }

    #[tokio::test]
    async fn runs_and_spools_output() {
        let dir = tempfile::tempdir().unwrap();
        let t = tracker(dir.path());
        let p = t
            .start(StartOptions {
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "echo out; echo err >&2; exit 3".into(),
                ],
                env: [("X".to_owned(), "y".to_owned())].into(),
                ..Default::default()
            })
            .unwrap();
        assert!(p.process_id.starts_with("proc-"));
        let done = wait_exit(&p).await;
        assert_eq!(done.state, proto::ProcessState::Exited as i32);
        assert_eq!(done.exit_code, 3);
        assert!(done.finished_at.is_some());
        assert_eq!(read_spool(&p.stdout_path, 0).unwrap().0, b"out\n");
        assert_eq!(read_spool(&p.stderr_path, 0).unwrap().0, b"err\n");
        assert_eq!(read_spool(&p.stdout_path, 2).unwrap(), (b"t\n".to_vec(), 4));
        assert_eq!(read_spool(&p.stdout_path, 10).unwrap(), (vec![], 10));
    }

    #[tokio::test]
    async fn stdin_and_signals() {
        let dir = tempfile::tempdir().unwrap();
        let t = tracker(dir.path());
        let cat = t
            .start(StartOptions {
                command: vec!["cat".into()],
                stdin: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(t.write_input(&cat.process_id, b"hi").await, Ok(2));
        t.close_input(&cat.process_id).await.unwrap();
        t.close_input(&cat.process_id).await.unwrap();
        assert_eq!(
            t.write_input(&cat.process_id, b"x").await,
            Err(Error::StdinClosed)
        );
        assert_eq!(wait_exit(&cat).await.exit_code, 0);
        assert_eq!(read_spool(&cat.stdout_path, 0).unwrap().0, b"hi");
        assert_eq!(
            t.write_input(&cat.process_id, b"x").await,
            Err(Error::Exited)
        );

        let sleeper = t
            .start(StartOptions {
                command: vec!["sleep".into(), "30".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            t.write_input(&sleeper.process_id, b"x").await,
            Err(Error::NoStdin)
        );
        t.signal(&sleeper.process_id, libc::SIGTERM).unwrap();
        assert_eq!(wait_exit(&sleeper).await.exit_code, 128 + 15);
        assert_eq!(
            t.signal(&sleeper.process_id, libc::SIGTERM),
            Err(Error::Exited)
        );
        assert_eq!(t.get("proc-missing").unwrap_err(), Error::NotFound);
    }

    #[tokio::test]
    async fn limits() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = TrackerConfig::with_log_dir(Some(dir.path().to_owned()));
        cfg.max_concurrent_processes = 1;
        cfg.max_log_bytes = 4;
        cfg.default_process_timeout = Duration::from_millis(200);
        let t = Tracker::new(cfg).unwrap();
        let p = t
            .start(StartOptions {
                command: vec!["sh".into(), "-c".into(), "echo 123456789; sleep 30".into()],
                ..Default::default()
            })
            .unwrap();
        assert!(matches!(
            t.start(StartOptions {
                command: vec!["true".into()],
                ..Default::default()
            }),
            Err(Error::TooManyProcesses(1))
        ));
        // The default timeout kills the whole group.
        assert_eq!(wait_exit(&p).await.exit_code, 128 + 9);
        let out = String::from_utf8(read_spool(&p.stdout_path, 0).unwrap().0).unwrap();
        assert!(
            out.starts_with("1234\n\n[guest: maximum log limit"),
            "{out}"
        );
        // A finished process frees its slot.
        let q = t
            .start(StartOptions {
                command: vec!["true".into()],
                ..Default::default()
            })
            .unwrap();
        wait_exit(&q).await;
    }

    #[tokio::test]
    async fn start_failures() {
        let dir = tempfile::tempdir().unwrap();
        let t = tracker(dir.path());
        let err = t
            .start(StartOptions {
                command: vec!["/nonexistent/binary".into()],
                ..Default::default()
            })
            .unwrap_err();
        assert!(err.to_string().starts_with("starting process"), "{err}");
        // The failed start did not use up a slot.
        for _ in 0..2 {
            t.start(StartOptions {
                command: vec!["sleep".into(), "1".into()],
                ..Default::default()
            })
            .unwrap();
        }
    }

    #[tokio::test]
    async fn prunes_finished_processes() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = TrackerConfig::with_log_dir(Some(dir.path().to_owned()));
        cfg.max_retained_processes = 1;
        let t = Tracker::new(cfg).unwrap();
        let mut ids = Vec::new();
        for _ in 0..3 {
            let p = t
                .start(StartOptions {
                    command: vec!["true".into()],
                    ..Default::default()
                })
                .unwrap();
            wait_exit(&p).await;
            ids.push((p.process_id.clone(), p.stdout_path.clone()));
        }
        t.prune_expired();
        assert_eq!(t.get(&ids[0].0).unwrap_err(), Error::NotFound);
        assert_eq!(t.get(&ids[1].0).unwrap_err(), Error::NotFound);
        assert!(!ids[0].1.exists());
        t.get(&ids[2].0).unwrap();
    }

    #[test]
    fn timeouts() {
        let d = |seconds, nanos| prost_types::Duration { seconds, nanos };
        assert_eq!(timeout_of(None), Some(Duration::ZERO));
        assert_eq!(timeout_of(Some(&d(2, 5))), Some(Duration::new(2, 5)));
        assert_eq!(timeout_of(Some(&d(-2, 0))), Some(Duration::ZERO));
        assert_eq!(timeout_of(Some(&d(1, -5))), None);
        assert_eq!(timeout_of(Some(&d(0, 1_000_000_000))), None);
    }
}
