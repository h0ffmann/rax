# RAX on minikube: status, footprint and requirements

What it costs to run RAX on a local minikube cluster with
[`just minikube`](local-kubernetes.md), what you need to try its
capabilities, and what you cannot try there. MicroK8s runs the same
workloads, so the cluster-side figures apply to it too; only the node's own
overhead differs.

## Status

| | |
|---|---|
| Recipes (`install`, `up`, `deploy`, `status`, `footprint`, `ax`, `e2e`, `down`) | Written and statically checked (shellcheck, `just --dry-run`) |
| A cluster brought up with them | **Not yet.** The environment they were written in cannot run a nested cluster |
| AX's E2E scenarios against RAX | Pass locally without Kubernetes (`just e2e-local`), not yet on a cluster |
| Figures on this page | Measured where marked, estimated otherwise; see [How the figures were taken](#how-the-figures-were-taken) |

Until someone runs it end to end, treat the cluster figures as a budget, not
a measurement. Once you have a cluster up, `just footprint minikube` reports
the real numbers (see [Measuring your own cluster](#measuring-your-own-cluster)),
and a PR replacing the estimates here is welcome.

## Minimum requirements

| | Minimum (estimate) | Recommended (the recipe defaults) | AX's full E2E suite |
|---|---|---|---|
| minikube node CPUs (`MINIKUBE_CPUS`) | 3 | 4 | 4, or 6 with `AX_WORKER_REPLICAS=4` as kind uses |
| minikube node memory (`MINIKUBE_MEMORY`) | 6g | 8g | 8g, or 12g with 4 workers |
| Disk the node uses | 10 GB | 40g (`MINIKUBE_DISK`) | 20 GB |
| Free disk on the host, besides the node | 15 GB | 25 GB | 25 GB |
| Host memory | 10 GiB | 16 GiB | 16 GiB |
| Kubernetes | 1.34 | v1.37.0 | v1.37.0 |

Why these numbers:

- **CPU.** The pods request 1.5 CPUs between them (below), so 2 CPUs
  schedule, but the 2 gVisor workers, Substrate's Postgres (limit 2 CPUs)
  and the first rollouts all compete for them, and a task's agent needs CPU
  of its own. 3 is the least we would try; 4 leaves room for a couple of
  tasks at once.
- **Memory.** The pods request about 2 GiB and should idle around 2 to
  3 GiB; each running task adds its sandbox, from a few hundred MiB to more
  than 1 GiB for an agent CLI such as Claude Code or Gemini CLI.
- **Disk.** About 6 GB of images, registry and volumes on the node after
  the first deploy, growing with every redeploy and every snapshot (below).
  With the `docker` driver the node is a container, so that space comes out
  of the host's Docker data directory.
- **Host memory.** The node's memory comes out of the host with the `docker`
  driver, and `deploy` builds RAX (cargo, release profile) and Substrate
  (Go) on the host at the same time as the cluster runs.

Also needed on the host, unchanged from [local-kubernetes.md](local-kubernetes.md#quick-start):
Linux (x86_64 or arm64), rootful Docker, Go, Cargo, git, curl, openssl and
`just`, port 5001 free, and network access to Docker Hub, GitHub, the Go
module proxy, crates.io and the Debian and Alpine package mirrors.

Smaller nodes: `MINIKUBE_CPUS=3 MINIKUBE_MEMORY=6g MINIKUBE_DISK=20g just up minikube`.
`AX_WORKER_REPLICAS=1` saves a worker, but then only one task runs at a time
and AX's E2E suite will likely time out.

## What runs in the cluster

Requests and limits come from the manifests `deploy` applies: Substrate
v0.6.0's kind profile, AX's `test/e2e/manifests` and
[`registry.yaml`](../hack/local/registry.yaml) (measured, by rendering them).
The Kubernetes rows are upstream defaults for a kubeadm control plane, which
minikube uses.

| Namespace | Workload | Replicas | CPU request / limit | Memory request / limit | Volume |
|---|---|---|---|---|---|
| kube-system | apiserver, controller-manager, scheduler, etcd, CoreDNS, kindnet, kube-proxy, storage-provisioner | 1 each | 850m / – | 220Mi / – | |
| ate-system | `postgres` (Substrate's state) + TLS reloader | 1 | 260m / 2 | 1056Mi / 2Gi | 1Gi PVC |
| ate-system | `atelet` (DaemonSet) + objectstore plugin | 1 | 100m / – | 256Mi / – | |
| ate-system | `ate-api-server` + objectstore plugin | 2 | – | – | |
| ate-system | `ate-idp-server` | 2 | – | – | |
| ate-system | `ate-controller` | 1 | – | – | |
| ate-system | `atenet-router` + Envoy | 1 | – | – | |
| ate-system | `rustfs` (snapshot storage) | 1 | – | – | 1Gi PVC |
| otel-system | `prometheus` | 1 | 100m / – | 128Mi / 512Mi | |
| otel-system | `opentelemetry-collector`, `jaeger` | 1 each | – | – | |
| podcertificate-controller-system | `podcertificate-controller` | 1 | – | – | |
| ax-system | `ax-workers` (gVisor; the WorkerPool sets no resources) | 2 | – | – | |
| ax-system | `ax-postgres` (AX's state) | 1 | 100m / 1 | 256Mi / 1Gi | 1Gi PVC |
| ax-system | `ax-server` (rax-server) | 1 | 100m / 1 | 128Mi / 1Gi | |
| ax-registry | `registry` | 1 | – | – | hostPath `/var/lib/ax-registry` |
| **Total** | 26 pods | | **≈1.5 CPU** | **≈2.0 GiB** | 3Gi of PVCs |

Most of Substrate runs without requests (BestEffort), so the totals are a
floor: Kubernetes schedules them anywhere and evicts them first under memory
pressure. The two Postgres instances and the server are the only pods with
limits. The workers have no limits either, so every running task can use the
whole node.

Estimated memory at idle: about 0.7 GiB for Kubernetes, 0.6 to 0.9 GiB for
Substrate's Go services, Envoy and RustFS, 0.3 to 0.5 GiB for the
observability stack, 0.2 to 0.3 GiB for both Postgres instances, and tens of
MiB for rax-server; 2 to 3 GiB in all.

## Images

Sizes for linux/amd64. *Download* is the compressed size pulled or pushed
(and stored in the local registry); *on the node* is what containerd keeps,
the compressed layers plus the unpacked filesystem, as `docker image ls`
reports it with the containerd image store.

### Built from this checkout

| Image | Download | On the node | How we got it |
|---|---|---|---|
| `rax-server` (debian:bookworm-slim + rax-server, 15.9 MB) | 34 MB | 135 MB | measured, without the `ca-certificates` layer (+~1 MB, estimate) |
| `rax-task-runner` (python:3.12-slim + rax-task-runner, 9.8 MB) | 47 MB | 190 MB | measured, without the apt layer |
| apt layer of the task-runner (git, curl, openssh-client, procps) | +~30 MB | +~105 MB | estimate, the same layer as AX's image |
| E2E variant (the runner + AX's sample repository) | +<1 MB | +<1 MB | estimate |
| **Total, RAX** | **≈110 MB** | **≈430 MB** | |

For comparison, AX's Go images built the same way (measured, without their
apt layers): `ax-server` on alpine:3.21 is 17 MB to download and 54 MB on
the node; `ax-task-runner` is 54 MB and 209 MB. The Rust binaries are
smaller than the Go ones (rax-server 15.9 MB against 27.8 MB, the runner
9.8 MB against 20.8 MB, the CLI 12.8 MB against 20.3 MB), but `rax-server`
sits on Debian slim (81 MB unpacked) where AX uses Alpine (8 MB), which makes
the RAX server image the larger of the two. A distroless or Alpine/musl base
would save about 80 MB on the node.

### Pulled or built for Substrate and AX

| Image | Download | On the node | How we got it |
|---|---|---|---|
| `postgres:18-alpine` (Substrate) | 121 MB | 433 MB | measured |
| `postgres:17-alpine` (AX) | 118 MB | 424 MB | measured |
| `otel/opentelemetry-collector-contrib:0.157.0` | 113 MB | 528 MB | measured |
| `prom/prometheus:v3.5.3` | 118 MB | 426 MB | measured |
| `rustfs/rustfs:1.0.0-beta.3` | 109 MB | 412 MB | measured |
| `jaegertracing/all-in-one:1.55` | 34 MB | 109 MB | measured |
| `registry:3` | 21 MB | 87 MB | measured |
| Envoy dataplane (`envoyproxy/envoy:v1.39.4` + Substrate's module) | 71 MB + module | 282 MB + module | base measured, module not built |
| Substrate's own images, built by `ko` on distroless: ateapi, atecontroller, ate-idp-server, atenet, atelet, objectstore-plugin, podcertcontroller, ateworker-gvisor | 150–250 MB | 0.5–0.8 GB | estimate, not built here |
| **Total, Substrate and AX** | **≈0.9 GB** | **≈3.3 GB** | |

### minikube itself

| | Download | Disk | How we got it |
|---|---|---|---|
| Node image `kicbase:v0.0.48` (`docker` driver), on the host | 512 MB | 1.84 GB | measured |
| Kubernetes v1.37 images, preloaded into the node | ~400 MB | ~1.2 GB | estimate |

## Disk, all together

| Where | What | Size |
|---|---|---|
| Node | Kubernetes images | ~1.2 GB |
| Node | Substrate and AX images | ~3.3 GB |
| Node | RAX images | ~0.4 GB |
| Node | Registry (`/var/lib/ax-registry`, compressed copies of everything `deploy` pushes) | ~0.4 GB, +~10 MB per redeploy |
| Node | Postgres and RustFS volumes | well under 1 GB to start; snapshots grow RustFS |
| **Node, after the first deploy** | | **≈6 GB (estimate)** |
| Host | minikube's node image | 1.84 GB (measured) |
| Host | Build base images: `rust:1-bookworm` 2.23 GB, `python:3.12-slim` 0.18 GB, `debian:bookworm-slim` 0.12 GB (measured); Substrate's Envoy builder (`rust:bookworm` + clang) ~2.5 GB (estimate) | ~5 GB |
| Host | Built images kept by Docker after pushing | ~0.6 GB |
| Host | BuildKit cache mounts (cargo registry and `target`) | ~1 GB (estimate) |
| Host | `target/` for the host CLI build (release, the three binaries) | 745 MB (measured) |
| Host | Substrate checkout (`.tmp/local/substrate`) and Go build cache | ~0.4 GB + 1–2 GB (estimate) |
| **Host, besides the node** | | **≈10–12 GB** |

Nothing is pruned automatically. `just down minikube` deletes the node and
everything in it; the host side stays until you run `docker system prune`
(add `--volumes` and `docker builder prune` for the caches) and delete
`.tmp/local` and `target/`.

## What you can and cannot try on minikube

| Capability | On minikube | Notes |
|---|---|---|
| CLI: models, tasks, apply/get/delete, logs | Yes | Through `just ax minikube …` |
| Tasks in gVisor sandboxes | Yes | Linux host; Docker Desktop on macOS untried |
| Suspend and resume (snapshots) | Yes, locally | Snapshots live in the in-cluster RustFS and go when the cluster goes |
| `ssh` into a task through the atenet router (mTLS) | Should | Never exercised outside kind |
| Several tasks at once | Up to `AX_WORKER_REPLICAS` (2) | Assuming one active task per worker, as Substrate assigns actors to worker pods; add CPU and memory per extra worker |
| Traces and metrics | Yes | OpenTelemetry collector, Prometheus and Jaeger come with Substrate's kind profile |
| Comparing against AX | Yes | `SERVER=ax`, `RUNNER=ax`, `CLI=ax` |
| AX's E2E suite on a cluster | Should | `just e2e minikube`; the first run on a real Substrate |
| Micro-VM sandboxes (kata, cloud-hypervisor) | No | Needs `/dev/kvm` in the node; the recipes do not wire it |
| Task images from private registries | No | No image credential provider; public images or the local registry only |
| Egress credential injection | No | `deploy` passes `--credential-provider={"enabled":false}` |
| Multi-node, failover, high availability | No | One node; the registry serves only its own node |
| Durable state | Partly | Postgres survives `deploy` (`RESET_DB=true` wipes it); `just down` deletes everything |
| Performance or load figures | No | One node with BestEffort pods and unbounded workers says little about a real cluster |

## Limitations of running RAX on minikube

The full list is in [local-kubernetes.md](local-kubernetes.md#limitations).
The ones that decide whether minikube fits:

- **Not run end to end yet**, so the first run will hit rough edges.
- **Heavy for a laptop.** Substrate's kind profile brings two Postgres
  instances, RustFS and a full observability stack, so 26 pods and about 5 GB
  of images before the first task. RAX's own share is under 0.5 GB of images
  and a few hundred MiB of memory; the rest is Substrate and Kubernetes.
- **Long first deploy.** The first `deploy` builds Substrate's Go images, its
  Envoy module (a Rust build in Docker) and RAX's images (two release builds
  of the workspace in Docker) and downloads about 3 GB of images. Later deploys rebuild only
  RAX.
- **Driver.** Use the `docker` (or `podman`) driver. Other drivers fall back
  to a `kubectl port-forward` for the registry, which is slow for Substrate's
  images, and Docker or Podman must be rootful: the gVisor workers run as
  root with `SYS_ADMIN`, `NET_ADMIN` and other capabilities and unconfined
  AppArmor and seccomp.
- **One node.** No `--nodes`; the registry answers only on its own node.
- **Unbounded workers.** AX's WorkerPool sets no resources, so a busy task
  can starve the control plane on a small node. Size the node for the tasks
  you run, or lower `AX_WORKER_REPLICAS`.
- **Disk grows.** Each redeploy pushes new image layers to the registry,
  which is never garbage-collected, and each snapshot lands in RustFS.
  `just down minikube && just minikube` starts over.

## Measuring your own cluster

```sh
minikube addons enable metrics-server -p rax   # optional, for live usage
just footprint minikube
```

`footprint` only reads. It prints the node's capacity and the CPU and memory
the pods request, live usage when a metrics API is installed, the size of
every image the pods run grouped by namespace (an image shared by namespaces
counts once), the disk under containerd, the registry, the volumes and the
kubelet on the node, the PVCs, and Docker's and the build directories' usage
on the host. It works on MicroK8s too (`just footprint microk8s`, which
reads the host's directories with sudo).

## How the figures were taken

On 2026-10-29, on an x86_64 Linux host (4 CPUs, 15 GiB) without the
ability to run a cluster or reach the Debian package mirrors:

- **Measured:** release builds of `rax-server`, `rax-task-runner` and `rax`
  from this checkout, and of AX's `ax-server`, `ax-task-runner` and `ax`
  (Go 1.25, `CGO_ENABLED=0`); images made of each Dockerfile's base plus the
  binary, without the apt step; every third-party image `deploy` uses and
  minikube's `kicbase`, pulled for linux/amd64; requests, limits, replicas
  and volumes from Substrate v0.6.0's kind manifests (rendered with
  kustomize) and AX's E2E manifests.
- **Estimated:** the apt layers, Substrate's `ko` images and Envoy module,
  minikube's Kubernetes images, idle memory, and everything that only a
  running cluster shows. arm64 images are of similar size.
