//! Serves gRPC and HTTP health checks on one port.

use axum::Router;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use rax_api::v1alpha1::ax_server::AxServer;
use tonic::service::Routes;
use tower::ServiceExt;

use crate::AxService;

/// Routes gRPC requests to the AX service and answers `GET /healthz` with
/// `200 ok`. Every other HTTP request gets 404.
pub fn router(service: AxService) -> Router {
    let grpc = Routes::new(AxServer::new(service))
        .prepare()
        .into_axum_router();
    Router::new()
        .route("/healthz", get(|| async { "ok\n" }))
        .fallback(move |req: Request| async move {
            if is_grpc(&req) {
                grpc.oneshot(req).await.into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        })
}

fn is_grpc(req: &Request) -> bool {
    req.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/grpc"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use std::sync::Arc;

    #[tokio::test]
    async fn healthz() {
        let app = router(AxService::new(
            Arc::new(rax_store::MemoryStore::new()),
            Default::default(),
        ));
        let res = app
            .clone()
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"ok\n");

        let res = app
            .oneshot(Request::get("/api/v1/tasks").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
