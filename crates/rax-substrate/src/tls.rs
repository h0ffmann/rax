//! TLS for connections to Substrate and to actors, built on rustls with the
//! ring provider.

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use hyper_util::rt::TokioIo;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// How to set up a TLS client.
#[derive(Debug, Clone, Default)]
pub struct TlsOptions {
    /// PEM certificates to trust. Without one, the Mozilla roots are trusted.
    pub ca_file: Option<PathBuf>,
    /// A PEM bundle with a client certificate chain and its private key.
    pub identity_file: Option<PathBuf>,
    /// Accept any server certificate. For development only.
    pub insecure_skip_verify: bool,
    /// ALPN protocols to offer, such as `h2` for gRPC.
    pub alpn: Vec<Vec<u8>>,
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Builds a rustls client configuration from `opts`.
pub fn client_config(opts: &TlsOptions) -> io::Result<Arc<ClientConfig>> {
    let provider = provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| invalid(e.to_string()))?;
    let builder = if opts.insecure_skip_verify {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipVerify(provider)))
    } else {
        let mut roots = RootCertStore::empty();
        match &opts.ca_file {
            Some(path) => {
                let certs = CertificateDer::pem_file_iter(path)
                    .and_then(|it| it.collect::<Result<Vec<_>, _>>())
                    .map_err(|e| invalid(format!("reading CA file {}: {e}", path.display())))?;
                let (added, _) = roots.add_parsable_certificates(certs);
                if added == 0 {
                    return Err(invalid(format!(
                        "failed to parse CA certificate from {}",
                        path.display()
                    )));
                }
            }
            None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        }
        builder.with_root_certificates(roots)
    };
    let mut config = match &opts.identity_file {
        Some(path) => {
            let read_err = |e: rustls_pki_types::pem::Error| {
                invalid(format!("reading {}: {e}", path.display()))
            };
            let certs = CertificateDer::pem_file_iter(path)
                .and_then(|it| it.collect::<Result<Vec<_>, _>>())
                .map_err(read_err)?;
            let key = PrivateKeyDer::from_pem_file(path).map_err(read_err)?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|e| invalid(format!("loading client identity {}: {e}", path.display())))?
        }
        None => builder.with_no_client_auth(),
    };
    config.alpn_protocols = opts.alpn.clone();
    Ok(Arc::new(config))
}

/// A stream that is plain TCP or TLS over TCP.
pub trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// Opens a TCP connection to `host:port`, and with `tls` set, runs a TLS
/// handshake for `server_name` (or `host` when it is `None`).
pub async fn connect(
    host: &str,
    port: u16,
    tls: Option<(&Arc<ClientConfig>, Option<&str>)>,
) -> io::Result<Box<dyn Io>> {
    let tcp =
        TcpStream::connect((host.trim_start_matches('[').trim_end_matches(']'), port)).await?;
    tcp.set_nodelay(true)?;
    let Some((config, server_name)) = tls else {
        return Ok(Box::new(tcp));
    };
    let name = server_name
        .unwrap_or(host)
        .trim_start_matches('[')
        .trim_end_matches(']');
    let name = ServerName::try_from(name.to_owned()).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("server name {name:?}: {e}"),
        )
    })?;
    let stream = TlsConnector::from(config.clone())
        .connect(name, tcp)
        .await?;
    Ok(Box::new(stream))
}

/// A connector for tonic and hyper that dials the URI's host and port, with
/// TLS when it is configured.
#[derive(Clone)]
pub struct Connector {
    tls: Option<Arc<ClientConfig>>,
    server_name: Option<String>,
}

impl Connector {
    pub fn plaintext() -> Self {
        Self {
            tls: None,
            server_name: None,
        }
    }

    /// Dials with TLS, verifying the server as `server_name` when it is set
    /// and as the URI's host otherwise.
    pub fn tls(config: Arc<ClientConfig>, server_name: Option<String>) -> Self {
        Self {
            tls: Some(config),
            server_name,
        }
    }
}

impl tower::Service<http::Uri> for Connector {
    type Response = TokioIo<Box<dyn Io>>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<Self::Response>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: http::Uri) -> Self::Future {
        let this = self.clone();
        Box::pin(async move {
            let host = uri.host().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("no host in {uri}"))
            })?;
            let default_port = if this.tls.is_some() { 443 } else { 80 };
            let port = uri.port_u16().unwrap_or(default_port);
            let tls = this.tls.as_ref().map(|c| (c, this.server_name.as_deref()));
            Ok(TokioIo::new(connect(host, port, tls).await?))
        })
    }
}

/// Accepts every server certificate, still checking handshake signatures.
#[derive(Debug)]
struct SkipVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for SkipVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_configs() {
        let cfg = client_config(&TlsOptions {
            alpn: vec![b"h2".to_vec()],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cfg.alpn_protocols, [b"h2".to_vec()]);
        client_config(&TlsOptions {
            insecure_skip_verify: true,
            ..Default::default()
        })
        .unwrap();
    }

    #[test]
    fn rejects_missing_or_empty_ca_files() {
        let missing = TlsOptions {
            ca_file: Some("/nonexistent/ca.pem".into()),
            ..Default::default()
        };
        assert!(
            client_config(&missing)
                .unwrap_err()
                .to_string()
                .contains("/nonexistent/ca.pem")
        );

        let dir = std::env::temp_dir().join(format!("rax-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.pem");
        std::fs::write(&empty, "not a certificate\n").unwrap();
        let err = client_config(&TlsOptions {
            ca_file: Some(empty.clone()),
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("failed to parse CA certificate"),
            "{err}"
        );
        std::fs::remove_file(empty).unwrap();
    }
}
