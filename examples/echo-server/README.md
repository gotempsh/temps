# temps-echo-server

A tiny, zero-dependency HTTP echo server used to demonstrate Temps deployments —
especially **multi-node** ones. Every request gets a JSON response reflecting:

- the **request** — method, path, query, headers, body
- the **environment** — including the node-identity variables Temps injects when
  an app runs across a multi-node cluster:
  - `TEMPS_NODE_NAME` — the node the replica is running on (e.g. `worker-1`)
  - `TEMPS_NODE_ID` — that node's id
  - `TEMPS_REPLICA` — the replica ordinal

Each request also emits a structured JSON log line to stdout, so requests show up
in **Temps log history** (filterable by container and node).

```jsonc
// GET /hello?name=temps  ->
{
  "service": "temps-echo",
  "hostname": "a1b2c3d4e5f6",
  "node": { "name": "worker-1", "id": "1", "replica": "2" },
  "request": {
    "method": "GET",
    "path": "/hello",
    "query": { "name": "temps" },
    "headers": { "host": "...", "user-agent": "..." }
  },
  "env": { "TEMPS_NODE_NAME": "worker-1", "GREETING": "hello", ... },
  "timestamp": "2026-06-28T14:55:00.000Z"
}
```

It listens on `PORT` (default **8080**) and responds `200` on every path, so `/`
works as a health check.

## Deploy to Temps

### Option A — from this git repo (recommended for the exercise)

Point a Temps project at the repository, set the **root directory** to
`examples/echo-server`, and Temps builds the `Dockerfile`:

1. New project → connect this repo → **Root directory:** `examples/echo-server`.
2. Environment settings:
   - **Replicas:** `3`
   - **Target nodes:** all (e.g. control-plane + workers)
   - **Anti-affinity:** on (spread one replica per node)
   - **Exposed port:** `8080`
3. (Optional) add env vars — e.g. `GREETING=hello-from-temps` — to see them
   echoed back per replica.
4. Deploy. Each replica reports its own `TEMPS_NODE_*` identity.

### Option B — by pre-built image

```sh
docker build -t temps-echo:latest examples/echo-server
# ...push to a registry the cluster can reach, then deploy by image with
# exposed_port=8080.
```

## Local run

```sh
node examples/echo-server/server.js
curl 'http://localhost:8080/hello?name=temps'
```

## Dev-cluster demo

`tools/dev-cluster/deploy-echo.sh` deploys a 3-replica echo across the local DinD
cluster (control-plane + 2 workers) and prints the per-replica node identity —
the end-to-end multi-node exercise this example was built for.
