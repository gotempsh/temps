# ADR 061: Worker proxy and host metrics, pulled by the control plane

## Status

Accepted — 2026-10-23

Related: ADR 017 (split processes), ADR 020 (agent mTLS), ADR 058 (worker
public ingress).

## Context

The Proxy and Server pages only ever showed node 0, the control plane. A
worker serving public traffic (ADR 058) counted nothing, and its CPU, memory,
disk and I/O were invisible. Only the control plane writes the metrics store,
and a worker is a less trusted party than the control plane: anything it says
must be bounded and must not be able to rewrite history.

## Decision

**Same counters on both proxies.** `ProxyMetrics` moves from `temps-proxy` to
`temps-metrics::proxy` (re-exported where it was). The worker's public HTTP,
HTTPS and tunnel-origin listeners record every request into one process-wide
instance: status class, project vs. other, total time to the last body byte,
upstream time to first header, and WebSocket/SSE sessions as streaming. The
private `*.temps.local` listener is not counted; it is workload-to-workload,
not edge traffic.

**Pull, over the agent channel.** The agent serves `GET /agent/metrics`
behind node-token auth (mTLS where configured), the same channel deployments
use. Its body is the cumulative counter snapshot, a random per-process
`instance_id`, and a host sample (the node collector in host scope) taken at
request time. On every `monitoring.scrape_interval_secs` tick the control
plane pulls each active or draining worker (5 s timeout, 8 at a time) and
stores the result under the worker's node id. Because the host is sampled
when pulled, workers are sampled at the plane's cadence, with no new setting.

**Receive-time stamping.** The report has no timestamp field. Every point is
stamped with the control plane's clock when the body was read, so a worker
with a wrong clock cannot backfill, move or future-date samples.

**Deltas on the plane.** The plane keeps each worker's previous snapshot and
stores the difference. A lost pull widens the next interval instead of losing
traffic; a new `instance_id` means the agent restarted and its counters are
taken whole; the first report after a plane restart only sets the baseline.

**Bounds.** The body is read to at most 16 KiB (larger is refused before
parsing). Proxy counters are fixed-size arrays with no names, so they can only
become the series the plane's own proxy writes. Host samples are kept only for
the 17 names in `HOST_METRIC_NAMES`, once each, finite, non-negative, and
percentages ≤ 100; no labels, engines or environments are read from the wire.
Refused samples are dropped and logged once per node (up to 5 names, 64
characters each), not every interval. A report with another format version or
a malformed `instance_id` is refused whole.

**Status board.** The plane keeps each worker's last outcome in memory:
`reporting`, `agent_too_old` (404/405), `unreachable`, `rejected` (401/403)
or `invalid`. It logs a change of state once. `GET /nodes/metrics/reporting`
exposes it so the console can say "agent too old" instead of showing an empty
chart.

**All nodes.** `GET /nodes/metrics/cluster` and `.../cluster/latest` combine
the selected nodes server-side: counters and byte counts are summed,
percentages are the ratio of summed parts (5xx/requests, used/total), latency
is the slowest node, and CPU and load are the mean. The Proxy and Server pages
get a node selector ("All nodes" plus each node by name) held in the URL.

## Backwards compatibility

- A single-node install has no worker rows: the pull loop does nothing, the
  selector is hidden, and both pages read node 0 exactly as before.
- An agent without the endpoint answers 404. That is `agent_too_old` on the
  board and one info log, never an error per interval. Nothing is stored for
  it, and the page says the agent is too old.
- The report format is versioned. Snapshot fields default to zero and unknown
  fields are ignored, so a newer agent can add counters without breaking an
  older plane.

## Consequences

- Worker traffic shows up within one scrape interval, at that granularity.
- Per-request proxy logs from workers are out of scope; only aggregate
  counters travel.
- Traffic served in the interval before a plane restart is lost. That is the
  same as the plane's own proxy sampler.
