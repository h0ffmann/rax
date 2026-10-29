# Running AX on minikube or MicroK8s

AX's own development cluster is [kind](https://kind.sigs.k8s.io/), set up by
`hack/setup-e2e-kind.sh` in AX. This repository adds the same setup for two
other light distributions, [minikube](https://minikube.sigs.k8s.io/) and
[MicroK8s](https://microk8s.io/), driven by [`just`](https://just.systems/)
recipes. Everything that runs in the cluster is built from source: Agent
Substrate, AX's task-runner, and the control plane, which is `rax-server`
from this checkout by default or AX's Go `ax-server` with `SERVER=ax`.

> **Status: not yet run end to end.** The scripts were written against AX's
> kind setup and Substrate v0.6.0's installer and are statically checked
> (shellcheck, `just --dry-run`), but no cluster has been brought up with
> them yet: the environment they were written in cannot give a nested
> cluster network access. Expect to hit rough edges on the first run, and see
> [Limitations](#limitations).

## Quick start

You need a Linux host (x86_64 or arm64) with Docker, Go, git, curl, openssl
and `just`, plus about 4 CPUs, 8 GiB of memory and 40 GB of disk for the
cluster. AX is expected next to this checkout (`../ax`); otherwise it is
cloned into `.tmp/local/ax`.

```sh
# minikube
just install minikube     # minikube and kubectl into ~/.local/bin
just up minikube          # a single-node cluster with Substrate's feature gates
just deploy minikube      # registry, Substrate, WorkerPool, Postgres, rax-server

# MicroK8s (Linux only; installs a snap and reconfigures the host, see below)
just install microk8s
just up microk8s
just deploy microk8s

# or all three steps at once
just minikube
just microk8s
```

The first `deploy` builds Substrate's images from source and takes a while.
Later runs skip Substrate unless its version changes.

Then use AX's CLI, built from the AX checkout, against the cluster:

```sh
just ax minikube apply -f ../ax/examples/task.yaml
just ax minikube get tasks
just status minikube

# or work directly with kubectl and the ax binary
source .tmp/local/minikube.env
kubectl -n ax-system logs deploy/ax-server
$AX_BIN get tasks
```

### Iterating on RAX

Change the code, then run `just deploy <distro>` again. It rebuilds the
`rax-server` image from this checkout, pushes it, and rolls out the new digest.
Substrate, the WorkerPool and the Postgres data are kept.

| Variable | Default | Effect |
|---|---|---|
| `SERVER` | `rax` | `ax` deploys AX's Go `ax-server` instead, for comparison against the same database |
| `RESET_DB` | `false` | `true` wipes AX's Postgres volume first |
| `AX_DIR` | `../ax`, else `.tmp/local/ax` | AX checkout for manifests, task-runner, CLI and e2e suite |
| `SUBSTRATE_DIR` | `.tmp/local/substrate` | Use your own Substrate checkout, as is |
| `SUBSTRATE_REF` | the version AX's `go.mod` pins | Substrate version for the managed clone |
| `FORCE_INSTALL_SUBSTRATE` | `false` | Reinstall Substrate even if that commit is installed |
| `AX_WORKER_REPLICAS` | `2` | gVisor workers in the `ax-workers` WorkerPool (kind uses 4) |
| `K8S_VERSION` | `v1.37.0` | Kubernetes version for minikube and the installed kubectl |
| `MINIKUBE_PROFILE` | `rax` | minikube profile and kube context name |
| `MINIKUBE_DRIVER` | `docker` | minikube driver |
| `MINIKUBE_CPUS`, `MINIKUBE_MEMORY`, `MINIKUBE_DISK` | `4`, `8g`, `40g` | Node size |
| `MINIKUBE_START_ARGS` | | Extra `minikube start` flags |
| `MICROK8S_CHANNEL` | `1.37/stable` | Snap channel for a new MicroK8s install |

The full list is at the top of [`hack/local/local.sh`](../hack/local/local.sh).

### AX's end-to-end suite

`deploy` builds AX's E2E task-runner image (AX's image plus a sample git
repository), so AX's suite can run against the local cluster:

```sh
RESET_DB=true just deploy minikube
just e2e minikube
just e2e minikube -test.run TestE2E_ModelCRUD
```

The suite runs from the AX checkout with this cluster's kubeconfig and
context; a kind setup in the same AX checkout is not touched.

### Tearing down

```sh
just down minikube        # deletes the minikube profile
just down microk8s        # removes AX, Substrate and the registry; MicroK8s stays
just uninstall microk8s   # snap remove --purge, and the /var/lib/kubelet link
```

## What `deploy` does

It follows AX's `hack/setup-e2e-kind.sh` step by step and reuses AX's
manifests from `test/e2e/manifests` unchanged:

1. **Registry.** [`hack/local/registry.yaml`](../hack/local/registry.yaml)
   runs `registry:3` on the node's network at port 5001, so the node pulls
   from `localhost:5001` just as kind's nodes do. The host pushes to
   `localhost:5001` as well: MicroK8s runs on the host, and minikube publishes
   the port (`--ports=127.0.0.1:5001:5001`). A `kind-registry` Service in
   `ate-system` stands in for kind's registry container, which Substrate's
   atelet pulls task images from.
2. **Substrate.** Substrate's `hack/install-ate-kind.sh deploy ate-system`
   with AX's flags, pointed at this cluster's context. Substrate's kind
   profile fits any single local node: in-cluster Postgres, RustFS for
   snapshots, and no cloud credential provider. The installed commit is
   recorded on the `ate-system` namespace, and a different one is uninstalled
   first.
3. **WorkerPool.** AX's `namespace.yaml` and `workerpool.yaml`, with the gVisor
   worker image built by `ko`.
4. **Images.** `ax-task-runner` (AX's E2E variant) and the server, pushed and
   deployed by digest, since Substrate only runs digest-pinned images. The
   `ax` CLI is built for the host into `.tmp/local/bin/ax`.
5. **Control plane.** AX's `postgres.yaml` and `ax-server.yaml`, the latter
   with the `rax-server` image. RAX accepts the same flags, environment and
   Go-style `--addr=:8080`, so the manifest needs no changes.

| | kind (AX) | minikube | MicroK8s |
|---|---|---|---|
| Cluster | Substrate's `create-kind-cluster.sh` | `minikube start --container-runtime=containerd` with the feature gates | the snap, with flags added to `/var/snap/microk8s/current/args/*` |
| Registry | `kind-registry` container next to the nodes | in-cluster, node port 5001, published on the host | in-cluster, host port 5001 |
| Node tuning | `docker exec` sysctls in the node container | `minikube ssh` sysctls in the node | sysctls on the host |
| Storage | kind's local-path | minikube's `standard` | the `hostpath-storage` addon |
| Database between runs | wiped every run | kept (`RESET_DB=true` to wipe) | kept |

## Limitations

### What still comes from AX (Go)

RAX ports AX's control plane only (see the [parity report](../README.md#parity-with-ax)).
Everything else in the cluster and on your machine is AX's or Substrate's
Go code, built from their checkouts:

- **Every task runs AX's `ax-task-runner`.** RAX has no task-runner yet, so
  the metadata server, workspace setup (git clones, files, skills) and the
  supervised command inside each sandbox are AX's.
- **The CLI is AX's `ax`**, including `ax ssh` and its tunnels.
- **The end-to-end suite is AX's** (`test/e2e`, Go).
- **Substrate is entirely Go** and is built from source on every version
  change, which needs Go and Docker on the host.

So an AX checkout is always required, and `just deploy` exercises RAX only as
the server between AX's CLI and AX's runner.

### RAX against AX's suite has not been run

RAX's own tests run against an in-memory Substrate. Running AX's E2E suite
against `rax-server` here is the first time RAX meets a real Substrate and
AX's CLI, so failures are expected. Some will come from the
[known differences](../README.md#known-differences): for example, condition
messages read `context: message` rather than gRPC's `rpc error: ...` text,
and tasks reconciled by RAX get different per-task template digests than
AX's. Use `SERVER=ax` to tell a RAX bug from a setup problem.

### Substrate's requirements

- **Kubernetes with pod certificates.** Substrate's pod certificate controller
  needs the `ClusterTrustBundle`, `ClusterTrustBundleProjection` and
  `PodCertificateRequest` feature gates and the `certificates.k8s.io/v1beta1`
  API, which are off by default. `just up` turns them on; `deploy` refuses a
  cluster without them. `ax-server`'s manifest mounts a `podCertificate` and a
  `clusterTrustBundle` projected volume, so the server cannot start without
  them either. Kubernetes 1.34 or newer is needed; v1.37.0 is the default.
- **A Substrate version with `hack/install-ate-kind.sh`.** The version AX
  pins (v0.6.0 at the time of writing) has it; Substrate's `main` branch no
  longer does, so `SUBSTRATE_REF=main` does not work here.
- **Docker on the host**, even for MicroK8s: the images are built with it,
  and Substrate's installer checks `docker info` in its kind profile.
- **gVisor only.** Micro-VM sandbox classes (kata with cloud-hypervisor) need
  `/dev/kvm` wired into the node, which these recipes do not do.
- **Kind-profile defaults.** No image credential provider, so tasks can only
  use public images or images in the local registry. Snapshots live in the
  in-cluster RustFS and go away with the cluster.

### Single node, local registry

- The registry answers only on its own node's `localhost:5001`, so
  multi-node clusters (`minikube --nodes`, MicroK8s clustering) are not
  supported. `deploy` warns when it sees more than one node.
- The registry is plain HTTP without authentication, and listens on
  port 5001 of every node interface. On MicroK8s the node is your host, so it
  is reachable from your network unless a firewall blocks it.
- Port 5001 must be free on the host. AX's and Substrate's kind setups use
  the same port for `kind-registry`, so stop that container first.
- Images are built for the node's architecture with `docker build --platform`.
  A node with a different architecture than the host needs Docker's
  emulation (binfmt/QEMU).

### minikube

- The `docker` and `podman` drivers publish the registry on the host. Other
  drivers (kvm2, qemu, hyperkit, VirtualBox) fall back to a
  `kubectl port-forward` during `deploy`, which is slow for Substrate's
  images. `--ports` only applies when the cluster is created, so a profile
  created some other way also gets the fallback.
- Rootless Docker or Podman is not supported: Substrate's workers are
  privileged gVisor pods.
- macOS with Docker Desktop should work with the docker driver but has not
  been tried; `fs.inotify` and proxy ARP are then set inside the minikube
  node, not on the Mac.

### MicroK8s

- Linux only, with classic confinement (the snap's default).
- `just up microk8s` changes the host, because MicroK8s's node is the host:
  - adds the feature gates to kube-apiserver, kubelet, kube-controller-manager
    and kube-scheduler, and `--runtime-config` to kube-apiserver, then
    restarts MicroK8s;
  - adds `certs.d/localhost:5001/hosts.toml` so containerd pulls the registry
    over HTTP;
  - links `/var/lib/kubelet` to `/var/snap/microk8s/common/var/lib/kubelet`,
    because atelet registers its device plugin through `/var/lib/kubelet`.
    If `/var/lib/kubelet` already exists for something else, atelet's
    device plugin does not register;
  - sets `net.ipv4.conf.all.proxy_arp=1`, `net.ipv6.conf.all.proxy_ndp=1`
    and `fs.inotify.max_user_instances=8192` on the host (not persisted
    across reboots; re-run `just up microk8s`);
  - enables the `dns` and `hostpath-storage` addons;
  - adds you to the `microk8s` group.
- `just down microk8s` keeps all of that. `just uninstall microk8s` removes
  the snap and the `/var/lib/kubelet` link, but not the sysctls or
  `/var/lib/ax-registry`.
- MicroK8s uses Calico, not kindnet as kind does. Substrate's gVisor
  networking relies on proxy ARP, and has only been exercised on kind.
- `install` keeps an existing MicroK8s on whatever channel it has; Kubernetes
  older than 1.34 cannot run Substrate.
