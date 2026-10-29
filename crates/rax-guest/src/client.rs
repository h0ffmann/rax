//! A client for the guest services, as `rax ssh` uses it: run a command,
//! feed it stdin and signals, and stream its output until it exits.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use rax_substrate::tls::Connector;
use rustls::ClientConfig;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tonic::metadata::MetadataValue;
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};

use crate::proto::{
    GetProcessRequest, ProcessState, Signal, SignalProcessRequest, StartProcessRequest,
    StreamProcessOutputRequest, WriteProcessInputRequest, process_output::Output,
    process_service_client::ProcessServiceClient,
};

/// Adds `ate-target-actor` to every call, which the atenet router needs to
/// pick the actor.
#[derive(Clone, Default)]
pub struct TargetActor(Option<MetadataValue<tonic::metadata::Ascii>>);

impl Interceptor for TargetActor {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        if let Some(v) = &self.0 {
            req.metadata_mut().append("ate-target-actor", v.clone());
        }
        Ok(req)
    }
}

type Process = ProcessServiceClient<InterceptedService<Channel, TargetActor>>;

/// A connection to a task's guest services.
#[derive(Clone)]
pub struct Client {
    process: Process,
}

/// What to run, and where its input and output go.
pub struct ExecOptions {
    pub command: Vec<String>,
    pub cwd: String,
    pub env: HashMap<String, String>,
    /// Copied to the command's stdin, which is closed when this ends. Without
    /// it the command reads an empty stdin.
    pub stdin: Option<Box<dyn AsyncRead + Send + Unpin>>,
    pub stdout: Box<dyn Write + Send>,
    pub stderr: Box<dyn Write + Send>,
    /// Each signal received is delivered to the command's process group.
    pub signals: Option<mpsc::Receiver<Signal>>,
}

impl Client {
    /// Connects to the guest at `target` (`host:port`), over TLS when `tls`
    /// is set (verifying the server as `server_name`), adding
    /// `ate-target-actor: <atespace>/<actor>` to every call when
    /// `target_actor` is set. The connection is made on first use.
    pub fn connect(
        target: &str,
        target_actor: Option<&str>,
        tls: Option<(Arc<ClientConfig>, String)>,
    ) -> Result<Self, String> {
        let target = target
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        let scheme = if tls.is_some() { "https" } else { "http" };
        let endpoint = Endpoint::from_shared(format!("{scheme}://{target}"))
            .map_err(|e| format!("connecting to guest at {target}: {e}"))?;
        let connector = match tls {
            Some((config, name)) => Connector::tls(config, Some(name)),
            None => Connector::plaintext(),
        };
        let channel = endpoint.connect_with_connector_lazy(connector);
        let actor = target_actor
            .map(|a| {
                a.parse()
                    .map_err(|_| format!("invalid target actor {a:?}"))
            })
            .transpose()?;
        Ok(Self {
            process: ProcessServiceClient::with_interceptor(channel, TargetActor(actor)),
        })
    }

    /// Runs a command and streams its output to the writers until it exits,
    /// returning its exit status.
    pub async fn exec(&self, opts: ExecOptions) -> Result<i32, String> {
        if opts.command.is_empty() {
            return Err("exec: command cannot be empty".into());
        }
        let mut process = self.process.clone();
        let started = process
            .start_process(StartProcessRequest {
                command: opts.command,
                cwd: opts.cwd,
                env: opts.env,
                stdin: opts.stdin.is_some(),
                timeout: None,
            })
            .await
            .map_err(|s| format!("starting process: {}", go_status(&s)))?
            .into_inner();
        let pid = started.process_id;

        // Input and signal forwarding stop when the command finishes.
        let mut forwarders = Vec::new();
        if let Some(stdin) = opts.stdin {
            forwarders.push(tokio::spawn(forward_stdin(
                self.process.clone(),
                pid.clone(),
                stdin,
            )));
        }
        if let Some(signals) = opts.signals {
            forwarders.push(tokio::spawn(forward_signals(
                self.process.clone(),
                pid.clone(),
                signals,
            )));
        }
        let result = self.stream(&pid, opts.stdout, opts.stderr).await;
        for f in forwarders {
            f.abort();
        }
        result
    }

    async fn stream(
        &self,
        pid: &str,
        mut stdout: Box<dyn Write + Send>,
        mut stderr: Box<dyn Write + Send>,
    ) -> Result<i32, String> {
        let mut process = self.process.clone();
        let mut stream = process
            .stream_process_output(StreamProcessOutputRequest {
                process_id: pid.to_owned(),
                follow: true,
                ..Default::default()
            })
            .await
            .map_err(|s| format!("streaming process output: {}", go_status(&s)))?
            .into_inner();
        // With follow set, the stream ends with the final process state.
        loop {
            let msg = stream
                .message()
                .await
                .map_err(|s| format!("receiving output stream: {}", go_status(&s)))?;
            let Some(msg) = msg else { break };
            match msg.output {
                Some(Output::Exit(p)) => return Ok(p.exit_code),
                Some(Output::Stdout(data)) => {
                    let _ = stdout.write_all(&data);
                    let _ = stdout.flush();
                }
                Some(Output::Stderr(data)) => {
                    let _ = stderr.write_all(&data);
                    let _ = stderr.flush();
                }
                None => {}
            }
        }
        // The stream closed without reporting an exit; poll instead.
        loop {
            let p = process
                .get_process(GetProcessRequest {
                    process_id: pid.to_owned(),
                })
                .await
                .map_err(|s| format!("getting process state: {}", go_status(&s)))?
                .into_inner();
            if p.state != ProcessState::Running as i32 {
                return Ok(p.exit_code);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Copies `r` to the process's stdin and closes it when `r` ends. Errors are
/// dropped: they mean the process exited or the connection is gone, which
/// the output stream reports.
async fn forward_stdin(mut process: Process, pid: String, mut r: Box<dyn AsyncRead + Send + Unpin>) {
    let (tx, rx) = mpsc::channel(4);
    let call = tokio::spawn(async move {
        let _ = process
            .write_process_input(tokio_stream::wrappers::ReceiverStream::new(rx))
            .await;
    });
    let mut buf = vec![0; 32 * 1024];
    loop {
        match r.read(&mut buf).await {
            Ok(n) if n > 0 => {
                let msg = WriteProcessInputRequest {
                    process_id: pid.clone(),
                    data: buf[..n].to_vec(),
                    close: false,
                };
                if tx.send(msg).await.is_err() {
                    return;
                }
            }
            _ => {
                let _ = tx
                    .send(WriteProcessInputRequest {
                        process_id: pid.clone(),
                        data: Vec::new(),
                        close: true,
                    })
                    .await;
                drop(tx);
                let _ = call.await;
                return;
            }
        }
    }
}

async fn forward_signals(mut process: Process, pid: String, mut signals: mpsc::Receiver<Signal>) {
    while let Some(sig) = signals.recv().await {
        let _ = process
            .signal_process(SignalProcessRequest {
                process_id: pid.clone(),
                signal: sig as i32,
            })
            .await;
    }
}

/// Formats a status the way grpc-go does, `rpc error: code = NotFound desc =
/// ...`, so messages read the same as AX's and scripts that match on them
/// keep working.
pub fn go_status(s: &Status) -> String {
    format!(
        "rpc error: code = {} desc = {}",
        go_code_name(s.code()),
        s.message()
    )
}

/// The name grpc-go gives a status code.
pub fn go_code_name(code: tonic::Code) -> &'static str {
    use tonic::Code::*;
    match code {
        Ok => "OK",
        Cancelled => "Canceled",
        Unknown => "Unknown",
        InvalidArgument => "InvalidArgument",
        DeadlineExceeded => "DeadlineExceeded",
        NotFound => "NotFound",
        AlreadyExists => "AlreadyExists",
        PermissionDenied => "PermissionDenied",
        ResourceExhausted => "ResourceExhausted",
        FailedPrecondition => "FailedPrecondition",
        Aborted => "Aborted",
        OutOfRange => "OutOfRange",
        Unimplemented => "Unimplemented",
        Internal => "Internal",
        Unavailable => "Unavailable",
        DataLoss => "DataLoss",
        Unauthenticated => "Unauthenticated",
    }
}
