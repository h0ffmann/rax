//! The filesystem service: files streamed in and out of the container in
//! chunks, optionally confined to one directory.

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;

use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::proto::{
    ReadFileRequest, ReadFileResponse, WriteFileRequest, WriteFileResponse,
    file_system_service_server::FileSystemService,
};

/// The chunk size for streamed reads (64 KB).
pub const DEFAULT_READ_BUFFER_SIZE: usize = 64 * 1024;
/// The directory file access is confined to by default.
pub const DEFAULT_WORKSPACE: &str = "/workspace";

/// Configures the filesystem service.
#[derive(Debug, Clone)]
pub struct Config {
    /// The directory every path must fall inside. `None` or `/` disables
    /// confinement; relative paths are then relative to the process's
    /// working directory.
    pub root_directory: Option<PathBuf>,
    /// Zero means [`DEFAULT_READ_BUFFER_SIZE`].
    pub read_buffer_size: usize,
}

impl Default for Config {
    /// Confined to `$WORKSPACE`, or [`DEFAULT_WORKSPACE`].
    fn default() -> Self {
        let root = std::env::var_os("WORKSPACE")
            .filter(|w| !w.is_empty())
            .map_or_else(|| PathBuf::from(DEFAULT_WORKSPACE), PathBuf::from);
        Self {
            root_directory: Some(root),
            read_buffer_size: DEFAULT_READ_BUFFER_SIZE,
        }
    }
}

/// The `FileSystemService` gRPC service.
#[derive(Debug, Clone)]
pub struct Service {
    root: Option<PathBuf>,
    read_buffer_size: usize,
}

impl Service {
    pub fn new(cfg: Config) -> Self {
        let root = cfg
            .root_directory
            .filter(|r| !r.as_os_str().is_empty() && r != Path::new("/"))
            .map(|r| clean(&r));
        Self {
            root,
            read_buffer_size: if cfg.read_buffer_size == 0 {
                DEFAULT_READ_BUFFER_SIZE
            } else {
                cfg.read_buffer_size
            },
        }
    }

    /// Resolves a request path against the root and checks that it stays
    /// inside it.
    fn resolve(&self, req: &str) -> Result<PathBuf, Status> {
        if req.is_empty() {
            return Err(Status::invalid_argument("file path cannot be empty"));
        }
        let path = Path::new(req);
        let target = match &self.root {
            Some(root) if !path.is_absolute() => clean(&root.join(path)),
            _ => clean(path),
        };
        if let Some(root) = &self.root
            && !target.starts_with(root)
        {
            return Err(Status::permission_denied(format!(
                "access denied: path {req:?} is outside sandbox root directory {:?}",
                root.display().to_string()
            )));
        }
        Ok(target)
    }
}

/// Lexically cleans a path the way Go's `filepath.Clean` does: no `.`
/// components, `..` resolved against what precedes it, and never above `/`.
pub fn clean(path: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) => {}
                _ => out.push(c),
            },
            c => out.push(c),
        }
    }
    if out.is_empty() {
        return PathBuf::from(".");
    }
    out.iter().collect()
}

fn mkdir_parent(path: &Path, req: &str) -> Result<(), Status> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() && dir != Path::new(".") => {
            std::fs::create_dir_all(dir).map_err(|e| {
                Status::internal(format!(
                    "failed to create parent directories for {req:?}: {e}"
                ))
            })
        }
        _ => Ok(()),
    }
}

type ReadStream = Pin<Box<dyn Stream<Item = Result<ReadFileResponse, Status>> + Send>>;

#[tonic::async_trait]
impl FileSystemService for Service {
    type ReadFileStream = ReadStream;

    async fn read_file(
        &self,
        req: Request<ReadFileRequest>,
    ) -> Result<Response<ReadStream>, Status> {
        let req = req.into_inner();
        let path = self.resolve(&req.path)?;
        // With a mode, a missing file is created empty instead of being an
        // error.
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        if req.mode != 0 {
            mkdir_parent(&path, &req.path)?;
            opts.write(true).create(true).mode(req.mode);
        }
        let mut file = opts.open(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                Status::not_found(format!("file {:?} not found", req.path))
            }
            std::io::ErrorKind::PermissionDenied => {
                Status::permission_denied(format!("permission denied reading {:?}", req.path))
            }
            _ => Status::internal(format!("failed to open file {:?}: {e}", req.path)),
        })?;

        let (tx, rx) = mpsc::channel(4);
        let size = self.read_buffer_size;
        tokio::task::spawn_blocking(move || {
            let mut buf = vec![0; size];
            loop {
                match file.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => {
                        let chunk = ReadFileResponse {
                            chunk: buf[..n].to_vec(),
                        };
                        if tx.blocking_send(Ok(chunk)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.blocking_send(Err(Status::internal(format!(
                            "failed to read file {:?}: {e}",
                            req.path
                        ))));
                        return;
                    }
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn write_file(
        &self,
        req: Request<Streaming<WriteFileRequest>>,
    ) -> Result<Response<WriteFileResponse>, Status> {
        let mut stream = req.into_inner();
        let mut file: Option<std::fs::File> = None;
        let mut req_path = String::new();
        let mut total = 0i64;
        while let Some(msg) = stream.message().await? {
            let f = match &mut file {
                Some(f) => f,
                None => {
                    req_path = msg.path.clone();
                    let path = self.resolve(&req_path)?;
                    mkdir_parent(&path, &req_path)?;
                    if msg.seek_offset < 0 {
                        return Err(Status::invalid_argument("seek_offset cannot be negative"));
                    }
                    // Without a seek offset the file is replaced; with one,
                    // existing content is kept and writing starts there.
                    let mut opts = std::fs::OpenOptions::new();
                    opts.write(true)
                        .create(true)
                        .truncate(msg.seek_offset == 0)
                        .mode(if msg.mode != 0 { msg.mode } else { 0o644 });
                    let mut f = opts.open(&path).map_err(|e| {
                        if e.kind() == std::io::ErrorKind::PermissionDenied {
                            Status::permission_denied(format!(
                                "permission denied opening {req_path:?}: {e}"
                            ))
                        } else {
                            Status::internal(format!("failed to create file {req_path:?}: {e}"))
                        }
                    })?;
                    if msg.seek_offset > 0 {
                        f.seek(SeekFrom::Start(msg.seek_offset as u64))
                            .map_err(|e| {
                                Status::internal(format!(
                                    "failed to seek in file {req_path:?}: {e}"
                                ))
                            })?;
                    }
                    file.insert(f)
                }
            };
            if !msg.chunk.is_empty() {
                f.write_all(&msg.chunk).map_err(|e| {
                    Status::internal(format!("failed to write to file {req_path:?}: {e}"))
                })?;
                total += msg.chunk.len() as i64;
            }
        }
        let Some(f) = file else {
            return Err(Status::invalid_argument("no file data received"));
        };
        f.sync_all()
            .map_err(|e| Status::internal(format!("failed to close file {req_path:?}: {e}")))?;
        Ok(Response::new(WriteFileResponse {
            bytes_written: total,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_like_go() {
        for (input, want) in [
            ("/a/b/../c", "/a/c"),
            ("/../x", "/x"),
            ("a/./b/", "a/b"),
            ("../a", "../a"),
            ("a/..", "."),
            ("/", "/"),
        ] {
            assert_eq!(clean(Path::new(input)), PathBuf::from(want), "{input}");
        }
    }

    #[test]
    fn confines_paths() {
        let svc = Service::new(Config {
            root_directory: Some("/workspace/".into()),
            read_buffer_size: 0,
        });
        assert_eq!(svc.resolve("a/b").unwrap(), PathBuf::from("/workspace/a/b"));
        assert_eq!(
            svc.resolve("/workspace").unwrap(),
            PathBuf::from("/workspace")
        );
        for bad in ["../etc/passwd", "/etc/passwd", "/workspace2/x"] {
            assert_eq!(
                svc.resolve(bad).unwrap_err().code(),
                tonic::Code::PermissionDenied,
                "{bad}"
            );
        }
        assert_eq!(
            svc.resolve("").unwrap_err().code(),
            tonic::Code::InvalidArgument
        );

        let open = Service::new(Config {
            root_directory: Some("/".into()),
            read_buffer_size: 0,
        });
        assert_eq!(
            open.resolve("/etc/passwd").unwrap(),
            PathBuf::from("/etc/passwd")
        );
    }
}
