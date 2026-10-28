//! HTTP readiness checks against actors, directly or through the router.

use std::path::{Path, PathBuf};
use std::time::Duration;

use http_body_util::Empty;
use hyper::body::Bytes;

use crate::tls::{self, TlsOptions};

/// The router's name on its TLS certificate.
pub const DEFAULT_ROUTER_SERVER_NAME: &str = "atenet-router.ate-system.svc";
/// The CA bundle that signs in-cluster service certificates.
pub const DEFAULT_SERVICE_DNS_CA_FILE: &str = "/run/servicedns-ca/trust-bundle.pem";
/// The pod's identity, presented to the router as a client certificate.
pub const DEFAULT_POD_IDENTITY_BUNDLE_FILE: &str =
    "/run/podidentity.podcert.ate.dev/credential-bundle.pem";

/// Sends GET requests and reports whether they answer 200 OK. HTTPS
/// requests verify the server as `server_name` and present the pod identity
/// when it is mounted.
#[derive(Debug, Clone)]
pub struct HttpProbe {
    pub timeout: Duration,
    pub server_name: String,
    pub ca_file: PathBuf,
    pub identity_file: PathBuf,
}

impl Default for HttpProbe {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            server_name: DEFAULT_ROUTER_SERVER_NAME.to_owned(),
            ca_file: DEFAULT_SERVICE_DNS_CA_FILE.into(),
            identity_file: DEFAULT_POD_IDENTITY_BUNDLE_FILE.into(),
        }
    }
}

fn existing(path: &Path) -> Option<PathBuf> {
    path.is_file().then(|| path.to_owned())
}

impl HttpProbe {
    /// Reports whether `GET url` answers 200 within the timeout.
    pub async fn get_ok(&self, url: &str, headers: &[(&str, &str)]) -> bool {
        match tokio::time::timeout(self.timeout, self.get(url, headers)).await {
            Ok(Ok(status)) => status == http::StatusCode::OK,
            Ok(Err(e)) => {
                tracing::debug!(url, error = %e, "readiness probe failed");
                false
            }
            Err(_) => false,
        }
    }

    async fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<http::StatusCode, Box<dyn std::error::Error + Send + Sync>> {
        let uri: http::Uri = url.parse()?;
        let host = uri.host().ok_or("no host in URL")?;
        let https = uri.scheme_str() == Some("https");
        let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
        let io = if https {
            // Read the files on every probe, so rotated certificates are used.
            let config = tls::client_config(&TlsOptions {
                ca_file: existing(&self.ca_file),
                identity_file: existing(&self.identity_file),
                insecure_skip_verify: false,
                alpn: vec![b"http/1.1".to_vec()],
            })?;
            tls::connect(host, port, Some((&config, Some(&self.server_name)))).await?
        } else {
            tls::connect(host, port, None).await?
        };
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(io)).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let authority = uri
            .authority()
            .map(|a| a.as_str())
            .unwrap_or(host)
            .to_owned();
        let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
        let mut req = http::Request::get(path).header(http::header::HOST, authority);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = sender
            .send_request(req.body(Empty::<Bytes>::new())?)
            .await?;
        Ok(resp.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serves one canned response per connection and returns the requests.
    async fn serve(status: &'static str) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let mut buf = vec![0; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
                let resp =
                    format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                let _ = s.write_all(resp.as_bytes()).await;
            }
        });
        (addr.to_string(), rx)
    }

    #[tokio::test]
    async fn reports_200_as_ready() {
        let (addr, mut reqs) = serve("200 OK").await;
        let probe = HttpProbe::default();
        let url = format!("http://{addr}/readyz?check=workspace");
        assert!(
            probe
                .get_ok(&url, &[("ate-target-actor", "default/t")])
                .await
        );
        let req = reqs.recv().await.unwrap().to_lowercase();
        assert!(
            req.starts_with("get /readyz?check=workspace http/1.1"),
            "{req}"
        );
        assert!(req.contains("ate-target-actor: default/t"), "{req}");
        assert!(req.contains(&format!("host: {addr}")), "{req}");
    }

    #[tokio::test]
    async fn reports_other_answers_as_not_ready() {
        let (addr, _reqs) = serve("503 Service Unavailable").await;
        let probe = HttpProbe {
            timeout: Duration::from_millis(500),
            ..Default::default()
        };
        assert!(!probe.get_ok(&format!("http://{addr}/readyz"), &[]).await);
        // Nothing listens on port 1.
        assert!(!probe.get_ok("http://127.0.0.1:1/readyz", &[]).await);
        assert!(!probe.get_ok("not a url", &[]).await);
    }
}
