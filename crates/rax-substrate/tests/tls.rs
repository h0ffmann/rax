//! The client over TLS, with a CA file, an authority override and a token.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use rax_substrate::ateapi::control_server::ControlServer;
use rax_substrate::testing::FakeSubstrate;
use rax_substrate::{Client, ClientOptions};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;
use tonic::transport::server::Connected;

/// A TLS stream tonic can serve.
struct Conn(TlsStream<TcpStream>);

impl Connected for Conn {
    type ConnectInfo = ();
    fn connect_info(&self) {}
}

impl AsyncRead for Conn {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Conn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// Serves the fake over TLS with a certificate for `api.ate-system.svc`,
/// and returns its address and the CA certificate in PEM.
async fn serve_tls(fake: &FakeSubstrate) -> (String, String) {
    let cert = rcgen::generate_simple_self_signed(vec!["api.ate-system.svc".to_owned()]).unwrap();
    let pem = cert.cert.pem();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![CertificateDer::from(cert.cert.der().to_vec())], key)
    .unwrap();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<std::io::Result<Conn>>();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let (acceptor, tx) = (acceptor.clone(), tx.clone());
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(tcp).await {
                    let _ = tx.send(Ok(Conn(tls)));
                }
            });
        }
    });
    let svc = ControlServer::new(fake.clone());
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
            .await
            .unwrap();
    });
    (addr, pem)
}

#[tokio::test]
async fn connects_over_tls_with_a_ca_file_and_token() {
    let fake = FakeSubstrate::new();
    let (addr, ca) = serve_tls(&fake).await;
    let dir = std::env::temp_dir().join(format!("rax-tls-client-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (ca_file, token_file) = (dir.join("ca.pem"), dir.join("token"));
    std::fs::write(&ca_file, ca).unwrap();
    std::fs::write(&token_file, "tls-token\n").unwrap();

    let opts = |authority: &str| ClientOptions {
        target: addr.clone(),
        authority: authority.to_owned(),
        token_file: token_file.display().to_string(),
        ca_file: ca_file.display().to_string(),
        ..Default::default()
    };
    let client = Client::connect(opts("api.ate-system.svc")).unwrap();
    client.ensure_atespace("secure").await.unwrap();
    assert_eq!(fake.state().calls_of("CreateAtespace"), ["secure"]);
    assert_eq!(
        fake.state().last_authorization.as_deref(),
        Some("Bearer tls-token")
    );

    // Without the authority override the certificate does not match
    // 127.0.0.1, and with no CA the self-signed certificate is untrusted.
    let fast = rax_substrate::RetryPolicy {
        attempts: 1,
        ..rax_substrate::RetryPolicy::fast()
    };
    let wrong_name = Client::connect(opts(""))
        .unwrap()
        .with_retry_policy(fast.clone());
    assert_eq!(
        wrong_name.ensure_atespace("x").await.unwrap_err().code(),
        tonic::Code::Unavailable
    );
    let mut no_ca = opts("api.ate-system.svc");
    no_ca.ca_file = "/nonexistent/ca.pem".into();
    assert!(
        Client::connect(no_ca).is_err(),
        "an unreadable CA file fails setup"
    );

    // Skipping verification connects anyway.
    let insecure = Client::connect(ClientOptions {
        insecure_tls: true,
        ca_file: String::new(),
        ..opts("")
    })
    .unwrap();
    insecure.ensure_atespace("insecure").await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
