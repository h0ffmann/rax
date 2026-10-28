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
| 2 | `rax-server`: gRPC API, in-memory and PostgreSQL stores, distributed locks | in review |
| 3 | Substrate client, controller and reconciliation | not started |
| 4 | `rax` CLI, `rax-task-runner`, tunnel, end-to-end parity tests | not started |

## Layout

| Crate | Go counterpart | Role |
|---|---|---|
| `crates/rax-api` | `pkg/apis/v1alpha1` | Generated types and gRPC service, YAML encoding, validation |
| `crates/rax-store` | `internal/store`, `internal/lock` | In-memory and PostgreSQL stores, in-memory and PostgreSQL locks |
| `crates/rax-server` | `internal/server`, `cmd/ax-server` | The gRPC service and `/healthz` on one port, and the `rax-server` binary |

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

`curl localhost:8080/healthz` answers `ok`. Until the Substrate client lands
(step 3), tasks are stored and reported as stored, without actors.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

The PostgreSQL tests run when `RAX_TEST_POSTGRES_URL` points at a database
they may wipe, for example `postgres://rax@localhost:5432/rax_test`.

## License

See [LICENSE](LICENSE) and [NOTICE](NOTICE). Parts derived from AX keep their
Apache 2.0 license ([LICENSE-APACHE](LICENSE-APACHE)).
