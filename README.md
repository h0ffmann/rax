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
| 1 | Cargo workspace, API types generated from `ax.proto`, YAML manifests, validation | in review |
| 2 | `rax-server`: gRPC API, in-memory and PostgreSQL stores, distributed locks | not started |
| 3 | Substrate client, controller and reconciliation | not started |
| 4 | `rax` CLI, `rax-task-runner`, tunnel, end-to-end parity tests | not started |

## Layout

| Crate | Go counterpart | Role |
|---|---|---|
| `crates/rax-api` | `pkg/apis/v1alpha1` | Generated types and gRPC service, YAML encoding, validation |

The proto is compiled in pure Rust with [protox](https://crates.io/crates/protox),
so building needs no `protoc`. [pbjson](https://crates.io/crates/pbjson-build)
generates serde impls that follow the protobuf JSON mapping, which is what
protojson gives the Go implementation, and YAML is bridged through it.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

## License

See [LICENSE](LICENSE) and [NOTICE](NOTICE). Parts derived from AX keep their
Apache 2.0 license ([LICENSE-APACHE](LICENSE-APACHE)).
