//! Tunnels from the CLI to a cluster: a background `kubectl port-forward` to
//! the AX server per Kubernetes context, remembered under `~/.ax/tunnels` so
//! later commands reuse it, and short-lived port-forwards for `rax ssh`.
//!
//! The state files have the same names and fields as AX's, so `ax` and `rax`
//! share each other's tunnels.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::LazyLock;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::{Deserialize, Serialize};

pub static PORT_FORWARD_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Forwarding from .*:(\d+)\s+->").unwrap());

/// A background tunnel, as saved in `<context>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TunnelInfo {
    pub context: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub service: String,
    pub port: u16,
    pub pid: i32,
    /// RFC 3339.
    pub created_at: String,
}

/// Where to find the AX server.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub server_url: String,
    pub context: String,
    /// Defaults to `$AX_SYSTEM_NAMESPACE`, then `ax-system`.
    pub namespace: String,
    /// Defaults to `ax-server`.
    pub service: String,
    /// A resource such as `pod/name`, overriding `svc/<service>`.
    pub target: String,
    /// Defaults to 8080.
    pub port: u16,
}

/// Maps a context name to a file name: anything but letters, digits, `-` and
/// `_` becomes `_`.
pub fn sanitize_context(name: &str) -> String {
    if name.is_empty() {
        return "default".into();
    }
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn home_dir() -> std::io::Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::other("$HOME is not defined"))
}

/// `$AX_HOME/tunnels`, or `~/.ax/tunnels`, created if missing.
pub fn tunnel_dir() -> std::io::Result<PathBuf> {
    let dir = match std::env::var_os("AX_HOME").filter(|d| !d.is_empty()) {
        Some(home) => PathBuf::from(home).join("tunnels"),
        None => home_dir()?.join(".ax").join("tunnels"),
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn state_path(dir: &Path, context: &str) -> PathBuf {
    dir.join(format!("{}.json", sanitize_context(context)))
}

pub fn get_tunnel(context: &str) -> Result<TunnelInfo, String> {
    let dir = tunnel_dir().map_err(|e| e.to_string())?;
    let path = state_path(&dir, context);
    let data = std::fs::read(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
    serde_json::from_slice(&data).map_err(|e| e.to_string())
}

pub fn save_tunnel(info: &TunnelInfo) -> std::io::Result<()> {
    let dir = tunnel_dir()?;
    let data = serde_json::to_vec_pretty(info)?;
    std::fs::write(state_path(&dir, &info.context), data)
}

/// Terminates the tunnel's process group and forgets it.
pub fn stop_tunnel(info: &TunnelInfo) {
    if info.pid > 0 {
        // SAFETY: kill has no memory-safety preconditions.
        unsafe {
            libc::kill(-info.pid, libc::SIGTERM);
            libc::kill(info.pid, libc::SIGTERM);
        }
    }
    if let Ok(dir) = tunnel_dir() {
        let _ = std::fs::remove_file(state_path(&dir, &info.context));
    }
}

pub fn stop_tunnel_by_context(context: &str) -> Result<(), String> {
    stop_tunnel(&get_tunnel(context)?);
    Ok(())
}

/// Every saved tunnel, in file name order. Unreadable files are skipped.
pub fn list_tunnels() -> Result<Vec<TunnelInfo>, String> {
    let dir = tunnel_dir().map_err(|e| e.to_string())?;
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    names.sort();
    Ok(names
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .filter_map(|d| serde_json::from_slice(&d).ok())
        .collect())
}

/// Whether `127.0.0.1:<port>/healthz` answers 200 within 400ms.
pub fn is_tunnel_healthy(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    http_get_status(
        SocketAddr::from(([127, 0, 0, 1], port)),
        "/healthz",
        Duration::from_millis(400),
    ) == Some(200)
}

fn http_get_status(addr: SocketAddr, path: &str, timeout: Duration) -> Option<u16> {
    let mut s = TcpStream::connect_timeout(&addr, timeout).ok()?;
    s.set_read_timeout(Some(timeout)).ok()?;
    s.set_write_timeout(Some(timeout)).ok()?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[derive(Deserialize)]
struct KubeConfig {
    #[serde(rename = "current-context", default)]
    current_context: String,
}

/// The Kubernetes context to use: `override`, `$KUBECONTEXT`, the
/// kubeconfig's `current-context`, or what `kubectl config current-context`
/// says.
pub fn current_context(override_: &str) -> Result<String, String> {
    if !override_.is_empty() {
        return Ok(override_.to_owned());
    }
    if let Ok(env) = std::env::var("KUBECONTEXT")
        && !env.is_empty()
    {
        return Ok(env);
    }
    let kubeconfig = match std::env::var("KUBECONFIG") {
        Ok(p) if !p.is_empty() => Some(PathBuf::from(p)),
        _ => home_dir().ok().map(|h| h.join(".kube").join("config")),
    };
    if let Some(path) = kubeconfig
        && !path.to_string_lossy().contains(':')
        && let Ok(data) = std::fs::read_to_string(&path)
        && let Ok(cfg) = serde_yaml_ng::from_str::<KubeConfig>(&data)
        && !cfg.current_context.is_empty()
    {
        return Ok(cfg.current_context);
    }
    let out = Command::new("kubectl")
        .args(["config", "current-context"])
        .stderr(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let ctx = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if out.status.success() && !ctx.is_empty() {
        Ok(ctx)
    } else {
        Err(format!("kubectl config current-context: {}", out.status))
    }
}

fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Resolves the AX server URL. An explicit URL or `$AX_SERVER` wins.
/// Otherwise, with a Kubernetes context, a background port-forward to the
/// server is reused or started; without one, `http://localhost:8080`.
pub fn ensure_server_url(mut opts: Options) -> Result<String, String> {
    if !opts.server_url.is_empty() {
        return Ok(opts.server_url);
    }
    if let Ok(env) = std::env::var("AX_SERVER")
        && !env.is_empty()
    {
        return Ok(env);
    }
    let context = match current_context(&opts.context) {
        Ok(c) if !c.is_empty() => c,
        _ => return Ok("http://localhost:8080".into()),
    };
    if opts.namespace.is_empty() {
        opts.namespace = std::env::var("AX_SYSTEM_NAMESPACE")
            .ok()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| rax_api::DEFAULT_SYSTEM_NAMESPACE.into());
    }
    if opts.service.is_empty() {
        opts.service = "ax-server".into();
    }
    if opts.port == 0 {
        opts.port = 8080;
    }

    if let Ok(existing) = get_tunnel(&context) {
        // Tunnels saved before namespace and service were recorded match any.
        let same_target = (existing.namespace.is_empty() || existing.namespace == opts.namespace)
            && (existing.service.is_empty() || existing.service == opts.service);
        if same_target && is_tunnel_healthy(existing.port) {
            return Ok(format!("http://127.0.0.1:{}", existing.port));
        }
        stop_tunnel(&existing);
    }

    let dir = tunnel_dir().map_err(|e| format!("creating tunnel directory: {e}"))?;
    let log_path = dir.join(format!("{}.log", sanitize_context(&context)));
    let log = std::fs::File::create(&log_path).map_err(|e| format!("opening tunnel log: {e}"))?;
    let target = if opts.target.is_empty() {
        format!("svc/{}", opts.service)
    } else {
        opts.target.clone()
    };
    let mut cmd = Command::new("kubectl");
    cmd.args(["--context", &context, "port-forward", "-n", &opts.namespace])
        .arg(&target)
        .arg(format!(":{}", opts.port))
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawning kubectl port-forward for context {context:?}: {e}"))?;
    let pid = child.id();
    let read_log = || {
        std::fs::read_to_string(&log_path)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };

    // Wait for kubectl to report the local port.
    let mut local_port = 0u16;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        if let Some(port) = PORT_FORWARD_REGEX
            .captures(&read_log())
            .and_then(|c| c[1].parse().ok())
            .filter(|p| *p > 0)
        {
            local_port = port;
            break;
        }
        if matches!(child.try_wait(), Ok(Some(_))) || !process_alive(pid) {
            return Err(format!(
                "kubectl port-forward exited for context {context:?}: {}",
                read_log()
            ));
        }
    }
    if local_port == 0 {
        let _ = child.kill();
        return Err(format!(
            "timeout waiting for kubectl port-forward on context {context:?}: {}",
            read_log()
        ));
    }

    let healthy_by = Instant::now() + Duration::from_secs(3);
    let mut healthy = false;
    while Instant::now() < healthy_by {
        if is_tunnel_healthy(local_port) {
            healthy = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !healthy {
        let _ = child.kill();
        return Err(format!(
            "tunnel started on port {local_port} but health check failed for context {context:?}"
        ));
    }

    let _ = save_tunnel(&TunnelInfo {
        context: context.clone(),
        namespace: opts.namespace,
        service: opts.service,
        port: local_port,
        pid: pid as i32,
        created_at: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false),
    });
    // The tunnel outlives this command on purpose.
    std::mem::forget(child);
    Ok(format!("http://127.0.0.1:{local_port}"))
}

/// A short-lived `kubectl port-forward`, stopped when dropped.
#[derive(Debug)]
pub struct PortForward {
    pub port: u16,
    child: Child,
}

impl Drop for PortForward {
    fn drop(&mut self) {
        // SAFETY: kill has no memory-safety preconditions.
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGTERM) };
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Port-forwards to a resource such as `svc/name` and returns once kubectl
/// reports the local port.
pub fn port_forward(
    kube_context: &str,
    namespace: &str,
    target: &str,
    remote_port: u16,
) -> Result<PortForward, String> {
    let context = current_context(kube_context).unwrap_or_default();
    let mut cmd = Command::new("kubectl");
    if !context.is_empty() {
        cmd.args(["--context", &context]);
    }
    if !namespace.is_empty() {
        cmd.args(["-n", namespace]);
    }
    cmd.args(["port-forward", target, &format!(":{remote_port}")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawning kubectl port-forward for {target}: {e}"))?;

    let (tx, rx) = mpsc::channel();
    for pipe in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(pipe);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                let _ = tx.send(Some(line.clone()));
                line.clear();
            }
            let _ = tx.send(None);
        });
    }
    drop(tx);

    let mut pf = PortForward { port: 0, child };
    let mut output = String::new();
    let mut open_pipes = 2;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Some(line)) => {
                output.push_str(&line);
                if let Some(port) = PORT_FORWARD_REGEX
                    .captures(&output)
                    .and_then(|c| c[1].parse().ok())
                    .filter(|p| *p > 0)
                {
                    pf.port = port;
                    return Ok(pf);
                }
            }
            Ok(None) => {
                open_pipes -= 1;
                if open_pipes == 0 {
                    return Err(format!("kubectl port-forward exited: {}", output.trim()));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!("kubectl port-forward exited: {}", output.trim()));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(format!("timed out waiting for port-forward to {target}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_contexts() {
        for (input, want) in [
            ("my-cluster", "my-cluster"),
            (
                "gke_project_us-central1_cluster",
                "gke_project_us-central1_cluster",
            ),
            (
                "arn:aws:eks:us-west-2:123456789:cluster/prod",
                "arn_aws_eks_us-west-2_123456789_cluster_prod",
            ),
            ("", "default"),
        ] {
            assert_eq!(sanitize_context(input), want);
        }
    }

    #[test]
    fn port_forward_regex() {
        for (log, port) in [
            ("Forwarding from 127.0.0.1:54947 -> 8080\n", "54947"),
            ("Forwarding from [::1]:61234 -> 8080\n", "61234"),
            (
                "Some prelude\nForwarding from 127.0.0.1:8080 -> 8080\n",
                "8080",
            ),
        ] {
            assert_eq!(&PORT_FORWARD_REGEX.captures(log).unwrap()[1], port);
        }
    }

    #[test]
    fn reads_go_state_files() {
        // A file as AX's Go CLI writes it.
        let info: TunnelInfo = serde_json::from_str(
            r#"{"context":"kind-ax","namespace":"ax-system","service":"ax-server","port":40123,"pid":4242,"created_at":"2026-10-29T20:00:00.123456789+01:00"}"#,
        )
        .unwrap();
        assert_eq!(info.port, 40123);
        // Older files without namespace and service still load.
        let old: TunnelInfo = serde_json::from_str(
            r#"{"context":"c","port":1,"pid":2,"created_at":"2026-10-29T20:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(old.namespace, "");
    }
}
