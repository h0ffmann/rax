# rax

Rustic Agentic Runtime: a pure-Rust port of [AX](https://github.com/google/ax),
the declarative orchestrator for running agentic tasks on
[Agent Substrate](https://github.com/agent-substrate/substrate).

RAX keeps AX's `ax.io/v1alpha1` API: the same gRPC service and the same
Task, Workspace and Model manifests, so a manifest written for AX works here.

## Status

The port lands in four steps, each its own pull request:

| Step | Scope | Status |
|---|---|---|
| 1 | Cargo workspace, API types generated from `ax.proto`, YAML manifests, validation | done |
| 2 | `rax-server`: gRPC API, in-memory and PostgreSQL stores, distributed locks | done |
| 3 | Substrate client, controller and reconciliation | done |
| 4 | `rax` CLI, `rax-task-runner`, tunnel, end-to-end parity tests | not started |

## Layout

| Crate | Go counterpart | Role |
|---|---|---|
| `crates/rax-api` | `pkg/apis/v1alpha1` | Generated types and gRPC service, YAML encoding, validation |
| `crates/rax-store` | `internal/store`, `internal/lock` | In-memory and PostgreSQL stores, in-memory and PostgreSQL locks |
| `crates/rax-substrate` | `internal/substrate` | The Substrate control API client (gRPC over rustls), actor templates, readiness probes, and an in-memory Substrate for tests |
| `crates/rax-server` | `internal/server`, `internal/controller`, `cmd/ax-server` | The gRPC service and `/healthz` on one port, the task reconciler, and the `rax-server` binary |

The PostgreSQL store uses AX's schema and the same protobuf JSON documents,
so RAX and AX can read each other's records.

The proto is compiled in pure Rust with [protox](https://crates.io/crates/protox),
so building needs no `protoc`. [pbjson](https://crates.io/crates/pbjson-build)
generates serde impls that follow the protobuf JSON mapping, which is what
protojson gives the Go implementation, and YAML is bridged through it.

## Running the server

```sh
# Everything in memory, for a quick local run:
cargo run -p rax-server -- --store memory:// --addr 127.0.0.1:8080

# Against PostgreSQL (the default store URL is postgres://ax@localhost:5432/ax):
AX_STORE=postgres://rax@localhost:5432/rax cargo run -p rax-server
```

`curl localhost:8080/healthz` answers `ok`.

## Running on a local cluster

`just` recipes bring up AX on a single-node minikube or MicroK8s cluster,
with Agent Substrate, AX's task-runner and `rax-server` all built from
source and deployed with AX's own manifests:

```sh
just minikube     # install minikube, create the cluster, deploy
just microk8s     # the same on MicroK8s (Linux)
just ax minikube get tasks
```

See [docs/local-kubernetes.md](docs/local-kubernetes.md) for the steps,
the settings, and the limitations: the CLI, task-runner and E2E suite are
still AX's Go code, Substrate needs pod-certificate feature gates, and only
single-node clusters with gVisor sandboxes are supported. The setup has not
been run end to end yet.

The server reconciles tasks on Agent Substrate. Inside the cluster the
defaults find it (`api.ate-system.svc.cluster.local:443`, with the mounted
service account token and trust bundle). Elsewhere, point it at Substrate
with `--substrate-endpoint`, and `--substrate-ca-file`,
`--substrate-token-file`, `--substrate-insecure-tls` or
`--substrate-plaintext` as needed. Like AX, each task gets an actor named
after it and a per-task actor template named `<task>-tmpl-<digest>`.
Setting `ATENET_ROUTER_ADDR` also probes workspace readiness through the
router.

## Parity with AX

**RAX does not cover all of AX yet.** The control plane is complete: everything
`ax-server` does, RAX's `rax-server` does too. What runs inside a task's
container (the task-runner) and on the user's machine (the `ax` CLI and its
tunnel) is step 4 and has not been ported. Counting AX's hand-written,
non-test Go, 3,857 of 7,597 lines (51%) have a Rust counterpart.

### Feature by feature

| AX package | What it does | RAX | Status |
|---|---|---|---|
| `pkg/apis/v1alpha1` (types, resources) | Task, Workspace and Model types, YAML and JSON encoding, name and spec validation, CPU and memory quantities | `rax-api` | done |
| `internal/store`, `store/memory`, `store/postgres` | Resource store; PostgreSQL with AX's schema | `rax-store` | done (no TLS to PostgreSQL yet) |
| `internal/lock`, `store/postgres/lock.go` | Per-resource locks; PostgreSQL leases with renewal and takeover | `rax-store` | done |
| `internal/server` | The `ax.v1alpha1.AX` gRPC API (all 14 RPCs), actor-owned status, `/healthz` | `rax-server` | done |
| `cmd/ax-server` | Server binary, flags and environment | `rax-server` binary | done (same flags) |
| `internal/substrate` | Substrate control API client: TLS, token, retries, atespaces, templates, actors | `rax-substrate` | done |
| `internal/controller` | Task reconciler: per-task templates, suspend, resume, workspace readiness, delete cleanup | `rax-server::controller` | done |
| `cmd/ax` | CLI: `apply`, `get`, `describe`, `suspend`, `resume`, `delete`, `ssh`, `ctx`/`context`, `tunnel list`/`stop`, `version` | none | not started (step 4) |
| `internal/tunnel` | CLI tunnels: per-context state, `kubectl port-forward`, router TLS | none | not started (step 4) |
| `internal/guest` | Client for the in-actor guest daemon (`ax ssh`: exec, stdin, signals) | none | not started (step 4) |
| `cmd/ax-task-runner`, `runner` | The process inside each task container: runs setup, then supervises the task's command | none | not started (step 4) |
| `internal/metadata` | In-container metadata server: `/healthz`, `/readyz?check=workspace`, `/metadata` task and workspace | none | not started (step 4) |
| `internal/workspace` | Workspace setup on first run: git clones with retry, files, skills, marker files | none | not started (step 4) |
| `internal/model` | Gemini client built from a Model resource (API key from secret or store) | none | not started (step 4) |
| `Dockerfile.*`, `deploy/`, `hack/`, `test/e2e` | Images, Kubernetes manifests, kind-based end-to-end tests | `Dockerfile.server`, `hack/local` | partial: a `rax-server` image and a minikube/MicroK8s setup that reuses AX's manifests and runs AX's suite; no task-runner image, no kind setup of its own |

Because the task-runner is missing, a task that RAX provisions runs AX's
`ax-task-runner` image (the default `--default-task-image`). The API,
manifests and PostgreSQL records are the same, so AX's `ax` CLI should work
against `rax-server`. That has not been tested yet.

### Known differences

- PostgreSQL connections have no TLS: `sslmode=require` fails, `disable` works.
- YAML is written with 2-space indentation where Go writes 4. A Task's
  `AX_TASK_YAML`, and with it the per-task template digest, therefore differs
  from AX's for the same task, so after switching from AX to RAX, each
  task gets a new template the first time it is reconciled.
- The in-memory store lists newest first; AX's sorts by key.
- Conditions show `context: message` rather than gRPC's
  `rpc error: code = ... desc = ...` text.
- Deleting an actor stops waiting after 2 minutes instead of relying only on
  the request deadline.
- Without a CA file, the Substrate client trusts the Mozilla roots
  (webpki-roots) rather than the system roots.

### What is tested

98 tests, all run in CI (`cargo test --all`, with a PostgreSQL 16 service).
They port the Go tests for the packages above and add some of their own.

| Suite | Tests | Covers |
|---|---|---|
| `rax-api` unit | 21 | YAML round trips and strict decoding (unknown fields rejected), lenient decoding, Workspace MCP and Model secret-key spellings, task config, workspace binding, name/meta/task/file validation, resource validation, CPU and memory parsing |
| `rax-store` unit | 11 | One conformance suite run on both the memory and the PostgreSQL store (CRUD, defaults, paging, not-found), concurrent schema creation, records with unknown fields, lock serialization, cancellation, lease renewal and takeover of expired leases, URL redaction |
| `rax-server` unit | 8 | Actor state to task phase and Ready, stored status keeps only AX's fields, `/healthz`, template naming and digest, the template name pattern, worker and router addresses, Go-style `:port` listen addresses |
| `rax-server` `tests/service.rs` | 15 | gRPC round trip over the real router, invalid names, workspace binding, reconcile lifecycle, delete (terminating, cleanup failure, status write failure), suspend success and failure, actor state read from Substrate, new tasks start suspended, failed and transiently failed provisioning, creation time kept, concurrent creates serialized |
| `rax-server` `tests/controller.rs` | 17 | Ports of AX's reconciler tests against an in-memory Substrate: actor named after the task, worker IP, default image, suspend and failed suspend, rejected template (bare Substrate message, no fallback), transient template failure, workspace readiness polled directly and through the router and kept across suspend and resume, no new template on status changes, resource limits and their failure paths, delete removes only the task's templates, waits for the actor, times out, transient failures and recovery; plus a full create, resume, get, suspend and delete through the service |
| `rax-substrate` unit | 12 | Retry and backoff rules, transient errors, template building, snapshot location, resource limits, TLS config, HTTP readiness probe |
| `rax-substrate` `tests/client.rs` | 13 | Client against an in-memory Substrate: atespace, template and actor creation, revert or recreate of crashed actors, waiting out deleting actors, resume retries (`ResourceExhausted` not retried), delete polling, missing actors and timeout, template deletion retries, pagination, token re-read on every call |
| `rax-substrate` `tests/tls.rs` | 1 | A real TLS connection: CA file, authority override, wrong server name rejected, unreadable CA, skip-verify |

Not tested yet: against a real Agent Substrate (only the in-memory one), an
AX and a RAX server sharing one database, and AX's end-to-end suite
(`test/e2e`). `just e2e <distro>` runs that suite, with AX's CLI and
task-runner, against `rax-server` on a local cluster
([docs/local-kubernetes.md](docs/local-kubernetes.md)), but it has not been
run yet.

## Development

```sh
cargo test                                   # or: just test
cargo clippy --all-targets -- -D warnings    # or: just lint
cargo fmt --all --check
```

The PostgreSQL tests run when `RAX_TEST_POSTGRES_URL` points at a database
they may wipe, for example `postgres://rax@localhost:5432/rax_test`.

## License

See [LICENSE](LICENSE) and [NOTICE](NOTICE). Parts derived from AX keep their
Apache 2.0 license ([LICENSE-APACHE](LICENSE-APACHE)).
