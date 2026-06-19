/**
 * Worker-pooled HTTP load generator. Fires a fixed number of requests (or runs
 * for a fixed duration) at a target URL with bounded concurrency, then reports
 * throughput, latency percentiles, status-code histogram and error rate.
 *
 * Pure fetch-based — no external load-test dependency. Designed to push tens of
 * thousands of requests from a single process by keeping `concurrency` in-flight
 * promises rather than spawning one promise per request up front.
 */

export interface LoadOptions {
  url: string
  /** Total requests to send. Ignored if `durationMs` is set. */
  requests?: number
  /** Run for this many ms instead of a fixed request count. */
  durationMs?: number
  /** Max in-flight requests. */
  concurrency: number
  method?: string
  headers?: Record<string, string>
  body?: string
  /** Per-request timeout in ms. */
  timeoutMs?: number
  /**
   * Retries for transient connection-level failures (socket reset/timeout with
   * no HTTP response). HTTP error statuses (4xx/5xx) are NOT retried — they are
   * real responses. Defaults to 2.
   */
  connectRetries?: number
  /** Called periodically with progress (completed count). */
  onProgress?: (completed: number, total: number | undefined) => void
}

export interface LoadResult {
  url: string
  sent: number
  completed: number
  errors: number
  /** requests/second over the wall-clock run. */
  rps: number
  durationMs: number
  latency: {
    minMs: number
    p50Ms: number
    p95Ms: number
    p99Ms: number
    maxMs: number
    meanMs: number
  }
  /** statusCode -> count. Network errors are bucketed under 0. */
  statusCodes: Record<number, number>
  /** True when every completed request returned a 2xx/3xx. */
  ok: boolean
}

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0
  const idx = Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))
  return sorted[idx] ?? 0
}

export async function runLoad(opts: LoadOptions): Promise<LoadResult> {
  const {
    url,
    concurrency,
    method = 'GET',
    headers,
    body,
    timeoutMs = 10_000,
    connectRetries = 2,
    durationMs,
  } = opts
  const totalRequests = durationMs ? undefined : (opts.requests ?? 1000)

  const latencies: number[] = []
  const statusCodes: Record<number, number> = {}
  let sent = 0
  let completed = 0
  let errors = 0

  const startWall = performance.now()
  const deadline = durationMs ? startWall + durationMs : undefined

  function shouldContinue(): boolean {
    if (deadline !== undefined) return performance.now() < deadline
    return sent < (totalRequests as number)
  }

  let lastProgressAt = 0
  function reportProgress() {
    if (!opts.onProgress) return
    const now = performance.now()
    if (now - lastProgressAt > 250) {
      lastProgressAt = now
      opts.onProgress(completed, totalRequests)
    }
  }

  async function attempt(): Promise<{ status: number; ms: number }> {
    const t0 = performance.now()
    const ctrl = new AbortController()
    const timer = setTimeout(() => ctrl.abort(), timeoutMs)
    try {
      const res = await fetch(url, { method, headers, body, signal: ctrl.signal })
      // Drain the body so the connection can be reused and timing is realistic.
      await res.arrayBuffer().catch(() => undefined)
      return { status: res.status, ms: performance.now() - t0 }
    } catch {
      // Connection-level failure (reset/abort/timeout) — no HTTP response.
      return { status: 0, ms: performance.now() - t0 }
    } finally {
      clearTimeout(timer)
    }
  }

  async function oneRequest(): Promise<void> {
    let result = await attempt()
    let totalMs = result.ms
    // Retry only transient connection failures (status 0). A real 4xx/5xx is a
    // genuine response and is recorded as-is.
    let retries = connectRetries
    while (result.status === 0 && retries > 0) {
      retries--
      result = await attempt()
      totalMs += result.ms
    }
    latencies.push(totalMs)
    statusCodes[result.status] = (statusCodes[result.status] ?? 0) + 1
    if (result.status === 0 || result.status >= 400) errors++
    completed++
    reportProgress()
  }

  // A worker pulls work until the stop condition is met. We run `concurrency`
  // workers; each keeps exactly one request in flight at a time, so total
  // in-flight === concurrency.
  async function worker(): Promise<void> {
    while (shouldContinue()) {
      sent++
      await oneRequest()
    }
  }

  await Promise.all(Array.from({ length: concurrency }, () => worker()))

  const wallMs = performance.now() - startWall
  latencies.sort((a, b) => a - b)
  const sum = latencies.reduce((acc, v) => acc + v, 0)

  return {
    url,
    sent,
    completed,
    errors,
    rps: completed / (wallMs / 1000),
    durationMs: wallMs,
    latency: {
      minMs: latencies[0] ?? 0,
      p50Ms: percentile(latencies, 50),
      p95Ms: percentile(latencies, 95),
      p99Ms: percentile(latencies, 99),
      maxMs: latencies[latencies.length - 1] ?? 0,
      meanMs: latencies.length ? sum / latencies.length : 0,
    },
    statusCodes,
    ok: errors === 0,
  }
}

/** Pretty one-block summary for human output. */
export function formatLoadResult(r: LoadResult): string {
  const codes = Object.entries(r.statusCodes)
    .sort(([a], [b]) => Number(a) - Number(b))
    .map(([code, n]) => `${code === '0' ? 'ERR' : code}:${n}`)
    .join('  ')
  const ms = (n: number) => `${n.toFixed(1)}ms`
  return [
    `  target      ${r.url}`,
    `  requests    ${r.completed} completed / ${r.sent} sent  (${r.errors} errors)`,
    `  throughput  ${r.rps.toFixed(0)} req/s  over ${(r.durationMs / 1000).toFixed(1)}s`,
    `  latency     p50 ${ms(r.latency.p50Ms)}  p95 ${ms(r.latency.p95Ms)}  p99 ${ms(r.latency.p99Ms)}  max ${ms(r.latency.maxMs)}`,
    `  status      ${codes}`,
  ].join('\n')
}
