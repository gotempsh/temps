// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * OTel ingestion + storage-quota enforcement against a live Temps instance --
 * proves real OTLP/HTTP protobuf payloads (traces/metrics/logs) actually land
 * through the ingest endpoints and are readable back through the real query
 * API and the unified Observe feed, then proves the quota cutoff documented
 * in `crates/temps-otel/src/services/otel_service.rs::check_quota` (born from
 * a real production incident: a 160GB/day OTel span flood before quota
 * enforcement existed) actually stops ingestion once a project's storage
 * crosses its configured limit -- not just that the config knob exists.
 *
 *   1. create a project. OTel's `tk_` (API-key) ingest path authenticates
 *      exactly like the harness's own control-plane calls -- a bearer `tk_`
 *      token plus `X-Temps-Project-Id` (`crates/temps-otel/src/ingest/auth.rs`'s
 *      `authenticate_api_key`) -- so this scenario ingests with the SAME
 *      `tk_` key the harness already authenticates with, rather than minting
 *      a fresh one: `POST /api-keys` is a `SensitiveAction::CreateApiKey`
 *      that the OSS `RequireVerificationAuthorizer` always downgrades to
 *      "needs interactive step-up verification" for EVERY principal type,
 *      including an already-authenticated API key -- by design, so a leaked
 *      `tk_` key can't mint itself more credentials for persistence. A
 *      scriptable e2e run has no session to step up with, so it uses the
 *      credential it was given, exactly as a real deployed collector would.
 *   2. hand-encode (no `@opentelemetry/*` SDK -- see `lib/otlp.ts`) and POST
 *      one real root span, one gauge metric point, and one log record as raw
 *      OTLP/HTTP protobuf to `/otel/v1/{traces,metrics,logs}`
 *   3. read them back through the REAL query API -- `GET /otel/traces`,
 *      `GET /otel/traces/{project_id}/{trace_id}`, `GET /otel/logs`,
 *      `GET /otel/metrics` -- asserting on decoded field values (span name,
 *      attributes, trace/span id, metric value, log body), not just "some
 *      row exists"
 *   4. confirm the root span shows up in the unified Observe feed
 *      (`GET /projects/{id}/observe/events?kinds=span`) -- the merge point
 *      `crates/temps-observability/src/handlers/events.rs` is supposed to
 *      expose alongside error/request/revenue signals
 *   5. quota: the target instance is launched with `TEMPS_OTEL_QUOTA_GB=1`
 *      (the smallest nonzero value the knob accepts -- see
 *      `crates/temps-otel/src/plugin.rs`), so quota enforcement is live from
 *      boot. Push large (~9 MB) trace batches in a loop, polling the
 *      UNCACHED `GET /otel/quota/{project_id}` admin endpoint after each one
 *      (ingest-time enforcement (`check_quota`) reuses a 30s-TTL cache --
 *      see `ingest/quota_cache.rs` -- so a burst inside that window can
 *      legitimately land after crossing 100%; this is a documented
 *      hot-path/accuracy tradeoff, not a bug) until usage_pct reaches >=100
 *   6. wait past the 30s ingest-time cache TTL, then push one more (small)
 *      batch carrying a unique canary span name and assert it is REJECTED
 *      with HTTP 413 (`OtelError::QuotaExceeded`) -- twice, to rule out a
 *      one-off flake -- and that the canary span never actually landed
 *      (queried back via `GET /otel/traces`, not just "the HTTP call
 *      errored")
 *   7. teardown: delete the project
 *
 * REAL BUG FOUND + FIXED (headline finding): quota enforcement was silently
 * inert for every project. `TimescaleDbStorage::get_storage_quota`
 * (`crates/temps-otel/src/storage/timescaledb.rs`) estimates a project's
 * share of each OTel hypertable as `table_size * (project_rows /
 * approximate_row_count(hypertable))`, but computed `table_size` via
 * `pg_total_relation_size('otel_spans')` on the hypertable's ROOT relation --
 * which holds no rows and almost no bytes of its own (TimescaleDB partitions
 * all actual data into child "chunk" tables under `_timescaledb_internal`),
 * so `table_size` never grew with real ingest volume. Verified live: with
 * `TEMPS_OTEL_QUOTA_GB=1`, pushing 2GB+ of real OTLP data into one project
 * never moved `usage_pct` past ~1.5%. Fixed by switching to
 * `hypertable_size('otel_spans'::regclass)`, TimescaleDB's chunk-aware
 * equivalent (still cheap -- proportional to chunk count, not row count).
 * After the fix, this scenario trips the quota consistently at ~110 batches
 * / ~990MB against a 1024MB configured limit across 4 consecutive clean
 * runs. Also added `LEAST(1.0, ratio)` around each per-table proportion:
 * `approximate_row_count` can lag the true count right after a write burst,
 * and without the clamp a stale denominator could compute a share above
 * 100% of the table.
 *
 * RESIDUAL LIMITATION found, NOT fixed here (documented, not silently
 * patched around): `approximate_row_count` is shared across every project on
 * the same hypertable, so a project checking quota shortly after a
 * DIFFERENT project's write burst can be told it's near/over 100% from a
 * handful of its own rows (reproduced live by running this scenario twice
 * back-to-back against the same database with no fresh project/DB in
 * between). Self-heals within seconds as autovacuum/ANALYZE catches up, and
 * doesn't recur with an isolated database per run (how this suite -- and CI
 * -- actually runs it). A proper fix (e.g. a periodic background ANALYZE of
 * the three OTel hypertables) is a bigger cost/design tradeoff than fits
 * this PR.
 *
 * Real bug found + fixed (secondary): `EventsQuery.kinds`'s doc comment
 * (surfaced into the OpenAPI spec via utoipa, and from there into
 * `@temps-sdk/api`'s generated docs) advertised `log` as a valid Observe
 * `kinds` filter value alongside request/span/error/revenue. It never was:
 * `ObservabilityEvent` has no `Log` variant (a deliberate design choice,
 * documented on the enum itself -- logs are too high-volume to interleave
 * with business signals) and `EventKind::parse("log")` returns `None`, so
 * `kinds=log` has always been rejected with 400 `InvalidKindsFilter`. Fixed
 * in `crates/temps-observability/src/handlers/events.rs` by correcting the
 * doc comment to match the code instead of leaving the code to eventually
 * "catch up" to a promise nothing implements. Step 4 asserts the real
 * (correct) behavior: `kinds=span` finds the span, `kinds=log` 400s.
 *
 * Real bug found + fixed (third, minor): `query_traces`'s utoipa
 * `#[utoipa::path(params(...))]` list (`crates/temps-otel/src/handlers/
 * query_handler.rs`) was missing `attributes` and `name_pattern` -- both
 * real, functioning query params on `TraceQueryParams` (and both already
 * correctly documented on the neighboring `query_trace_summaries` handler),
 * silently absent from the generated OpenAPI spec and therefore from
 * `@temps-sdk/api`'s typed `queryTraces()` signature. A real capability the
 * typed SDK couldn't express. Fixed by adding both to the params list.
 */
import {
  queryTraces,
  getTrace,
  queryLogs,
  queryMetrics,
  getQuota,
  observabilityListEvents,
} from '@temps-sdk/api'
import { makeClient, normalizeApiUrl, resolveConfig, unwrap } from '../lib/client.ts'
import { createE2eProject, teardown, makeRunId, sleep } from '../lib/flows.ts'
import {
  buildTraceExportRequest,
  buildMetricsExportRequest,
  buildLogsExportRequest,
  defaultResourceAttrs,
  randomTraceId,
  randomSpanId,
  randomFillerAscii,
  sendOtlp,
} from '../lib/otlp.ts'

export interface OtelQuotaScenarioOptions {
  keep?: boolean
  json?: boolean
  /** Max ingest batches to push while hunting for the quota cutoff (safety cap). */
  maxQuotaBatches?: string
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface OtelQuotaScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
  quota?: {
    limitBytes: number
    bytesSentByClientAtTrip: number
    reportedTotalBytesAtTrip: number
    batchesSent: number
  }
}

/** `check_quota`'s ingest-time cache TTL (`ingest/quota_cache.rs::QUOTA_CACHE_TTL`), plus margin. */
const QUOTA_CACHE_TTL_MS = 30_000
const QUOTA_WAIT_MARGIN_MS = 5_000

/** Bytes of filler-attribute payload per over-quota-hunting batch (stays under the 10 MiB decompressed / 12 MiB body cap). */
const BATCH_FILLER_BYTES = 9 * 1024 * 1024

export async function otelQuotaScenarioCommand(opts: OtelQuotaScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const apiBaseUrl = normalizeApiUrl(cfg.url)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps otel-quota scenario  ->  ${cfg.url}`)

  const runId = makeRunId(Date.now())
  const maxQuotaBatches = Number(opts.maxQuotaBatches ?? 250)
  const steps: StepLog[] = []
  const step = async <T>(name: string, fn: () => Promise<T>): Promise<T> => {
    const t0 = performance.now()
    log(`\n▶ ${name}`)
    try {
      const r = await fn()
      const ms = performance.now() - t0
      steps.push({ step: name, ok: true, ms })
      log(`  ✓ ${name} (${(ms / 1000).toFixed(1)}s)`)
      return r
    } catch (e) {
      const ms = performance.now() - t0
      steps.push({ step: name, ok: false, detail: (e as Error).message, ms })
      log(`  ✗ ${name}: ${(e as Error).message}`)
      throw e
    }
  }

  const projectIds: number[] = []
  let quotaSummary: OtelQuotaScenarioResult['quota']

  try {
    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-otel-quota` }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    // OTel's tk_ ingest auth path is checked against the SAME api_keys row
    // the harness itself authenticates with (see this file's header comment
    // for why a fresh key isn't minted here) -- so this is that key,
    // scoped to this project via X-Temps-Project-Id per request.
    const ingestKey = cfg.apiKey

    const resourceAttrs = defaultResourceAttrs(`${runId}-app`, { 'e2e.run_id': runId })
    const traceId = randomTraceId()
    const rootSpanId = randomSpanId()
    const rootSpanName = `otel-quota-e2e-root-${runId}`
    const metricName = `otel_quota_e2e_gauge_${runId.replace(/[^a-zA-Z0-9_.:-]/g, '_')}`
    const metricValue = 4242
    const logBody = `otel-quota-e2e log line ${runId}`

    await step('POST a real OTLP trace batch (one root span) to /otel/v1/traces', async () => {
      const payload = buildTraceExportRequest({
        resourceAttrs,
        spans: [
          {
            traceId,
            spanId: rootSpanId,
            name: rootSpanName,
            attributes: { 'e2e.run_id': runId, 'e2e.marker': 'trace' },
            statusCode: 1, // OK
          },
        ],
      })
      const res = await sendOtlp({ apiBaseUrl, apiKey: ingestKey, projectId: project.id, signal: 'traces', payload })
      if (res.status !== 200) throw new Error(`ingest traces returned HTTP ${res.status}: ${res.bodyText}`)
    })

    await step('POST a real OTLP metrics batch (one gauge point) to /otel/v1/metrics', async () => {
      const payload = buildMetricsExportRequest({
        resourceAttrs,
        metrics: [
          {
            name: metricName,
            unit: '1',
            dataPoints: [{ asDouble: metricValue, attributes: { 'e2e.run_id': runId } }],
          },
        ],
      })
      const res = await sendOtlp({ apiBaseUrl, apiKey: ingestKey, projectId: project.id, signal: 'metrics', payload })
      if (res.status !== 200) throw new Error(`ingest metrics returned HTTP ${res.status}: ${res.bodyText}`)
    })

    await step('POST a real OTLP logs batch (one record, correlated to the span) to /otel/v1/logs', async () => {
      const payload = buildLogsExportRequest({
        resourceAttrs,
        records: [
          {
            body: logBody,
            attributes: { 'e2e.run_id': runId },
            traceId,
            spanId: rootSpanId,
          },
        ],
      })
      const res = await sendOtlp({ apiBaseUrl, apiKey: ingestKey, projectId: project.id, signal: 'logs', payload })
      if (res.status !== 200) throw new Error(`ingest logs returned HTTP ${res.status}: ${res.bodyText}`)
    })

    await step('the span round-trips through GET /otel/traces (query by trace_id)', async () => {
      const res = unwrap(
        await queryTraces({ client, query: { project_id: project.id, trace_id: traceId } }),
        'queryTraces',
      )
      if (res.data.length !== 1) throw new Error(`expected 1 span for trace_id=${traceId}, got ${res.data.length}`)
      const span = res.data[0]!
      if (span.name !== rootSpanName) throw new Error(`span.name="${span.name}", expected "${rootSpanName}"`)
      if (span.span_id !== rootSpanId) throw new Error(`span.span_id="${span.span_id}", expected "${rootSpanId}"`)
      if (span.parent_span_id) throw new Error(`span.parent_span_id="${span.parent_span_id}", expected root (null/empty)`)
      if (span.attributes['e2e.run_id'] !== runId) throw new Error(`span attributes did not round-trip: ${JSON.stringify(span.attributes)}`)
      if (span.status_code !== 'OK') throw new Error(`span.status_code="${span.status_code}", expected OK`)
    })

    await step('the same trace round-trips through GET /otel/traces/{project_id}/{trace_id}', async () => {
      const res = unwrap(
        await getTrace({ client, path: { project_id: project.id, trace_id: traceId } }),
        'getTrace',
      )
      if (res.data.length !== 1 || res.data[0]!.trace_id !== traceId) {
        throw new Error(`getTrace did not return the expected single span: ${JSON.stringify(res.data)}`)
      }
    })

    await step('the log round-trips through GET /otel/logs (correct body + trace correlation)', async () => {
      const res = unwrap(
        await queryLogs({ client, query: { project_id: project.id, trace_id: traceId } }),
        'queryLogs',
      )
      if (res.data.length !== 1) throw new Error(`expected 1 log for trace_id=${traceId}, got ${res.data.length}`)
      const record = res.data[0]!
      if (record.body !== logBody) throw new Error(`log.body="${record.body}", expected "${logBody}"`)
      if (record.span_id !== rootSpanId) throw new Error(`log.span_id="${record.span_id}", expected "${rootSpanId}"`)
    })

    await step('the metric point round-trips through GET /otel/metrics with the exact value', async () => {
      const res = unwrap(
        await queryMetrics({ client, query: { project_id: project.id, metric_name: metricName } }),
        'queryMetrics',
      )
      if (res.data.length < 1) throw new Error(`expected at least 1 bucket for metric_name=${metricName}, got 0`)
      const bucket = res.data[0]!
      const value = bucket.value ?? bucket.avg_value
      if (value !== metricValue) throw new Error(`metric bucket value=${value}, expected ${metricValue}`)
    })

    await step('the root span shows up in the unified Observe feed (kinds=span)', async () => {
      const res = unwrap(
        await observabilityListEvents({
          client,
          path: { project_id: project.id },
          query: { kinds: 'span', search: rootSpanName },
        }),
        'observabilityListEvents',
      )
      const match = res.events.find((e) => e.type === 'span' && e.trace_id === traceId)
      if (!match) {
        throw new Error(`root span not found in Observe feed: ${JSON.stringify(res.events)}`)
      }
    })

    await step('kinds=log is rejected 400 (no Log variant in the Observe feed, by design -- not silently ignored)', async () => {
      const res = await observabilityListEvents({
        client,
        path: { project_id: project.id },
        query: { kinds: 'log' },
      })
      const status = res.response?.status
      if (status !== 400) {
        throw new Error(`observe/events?kinds=log returned HTTP ${status}, expected 400`)
      }
    })

    // ── Quota enforcement ────────────────────────────────────────────
    //
    // The target instance is expected to be launched with
    // TEMPS_OTEL_QUOTA_GB=1 (see this file's header comment) so the quota
    // is live for every project from boot, including this one.

    const initialQuota = await step('quota starts well under 100% for a fresh project', async () => {
      const res = unwrap(await getQuota({ client, path: { project_id: project.id } }), 'getQuota')
      log(`  limit_bytes=${res.quota.limit_bytes} total_bytes=${res.quota.total_bytes} usage_pct=${res.quota.usage_pct.toFixed(4)}`)
      if (res.quota.limit_bytes <= 0) {
        throw new Error(
          `quota.limit_bytes=${res.quota.limit_bytes} -- the target instance must be launched with TEMPS_OTEL_QUOTA_GB set (>=1) for this scenario to mean anything`,
        )
      }
      if (res.quota.usage_pct >= 100) {
        throw new Error(`quota.usage_pct=${res.quota.usage_pct} already >= 100 before any volume was pushed -- unclean project`)
      }
      return res.quota
    })

    let bytesSentByClient = 0
    let batchesSent = 0
    let trippedAtBytes: number | undefined
    let trippedAtReportedTotal: number | undefined

    await step(
      `push large OTLP trace batches (~${(BATCH_FILLER_BYTES / 1024 / 1024).toFixed(1)}MB each) until quota.usage_pct >= 100 (uncached GET /otel/quota read), capped at ${maxQuotaBatches} batches`,
      async () => {
        // High-entropy, not a repeated character: Postgres TOASTs large
        // JSONB attribute values through pglz, which would crush a repeated
        // byte down to a few KB regardless of BATCH_FILLER_BYTES and make
        // the quota's hypertable_size()-based math look wrong for a reason
        // that has nothing to do with the quota code (see randomFillerAscii's
        // doc comment).
        const filler = randomFillerAscii(BATCH_FILLER_BYTES)
        for (let i = 0; i < maxQuotaBatches; i++) {
          const payload = buildTraceExportRequest({
            resourceAttrs,
            spans: [
              {
                traceId: randomTraceId(),
                spanId: randomSpanId(),
                name: `otel-quota-e2e-filler-${runId}-${i}`,
                attributes: { 'e2e.run_id': runId, filler },
              },
            ],
          })
          const res = await sendOtlp({ apiBaseUrl, apiKey: ingestKey, projectId: project.id, signal: 'traces', payload })
          batchesSent++
          if (res.status === 200) {
            bytesSentByClient += payload.byteLength
          } else if (res.status !== 413) {
            throw new Error(`unexpected ingest status while pushing volume: HTTP ${res.status}: ${res.bodyText}`)
          }

          const q = unwrap(await getQuota({ client, path: { project_id: project.id } }), 'getQuota')
          log(
            `  batch ${i + 1}: ingest=${res.status} sent_so_far=${(bytesSentByClient / 1024 / 1024).toFixed(1)}MB ` +
              `reported_total=${(q.quota.total_bytes / 1024 / 1024).toFixed(1)}MB usage_pct=${q.quota.usage_pct.toFixed(2)}`,
          )
          if (q.quota.usage_pct >= 100 || res.status === 413) {
            trippedAtBytes = bytesSentByClient
            trippedAtReportedTotal = q.quota.total_bytes
            return
          }
        }
        throw new Error(
          `quota never reached 100% after ${maxQuotaBatches} batches (~${(bytesSentByClient / 1024 / 1024).toFixed(0)}MB sent, limit=${initialQuota.limit_bytes} bytes)`,
        )
      },
    )

    quotaSummary = {
      limitBytes: initialQuota.limit_bytes,
      bytesSentByClientAtTrip: trippedAtBytes ?? bytesSentByClient,
      reportedTotalBytesAtTrip: trippedAtReportedTotal ?? 0,
      batchesSent,
    }
    log(
      `  quota crossed 100% after ~${((trippedAtBytes ?? 0) / 1024 / 1024).toFixed(1)}MB client-sent ` +
        `(configured limit ${(initialQuota.limit_bytes / 1024 / 1024).toFixed(0)}MB) in ${batchesSent} batch(es)`,
    )

    await step(
      `wait out the ${(QUOTA_CACHE_TTL_MS / 1000).toFixed(0)}s ingest-time quota cache TTL so the next ingest call re-checks storage live`,
      () => sleep(QUOTA_CACHE_TTL_MS + QUOTA_WAIT_MARGIN_MS),
    )

    const canarySpanName = `otel-quota-e2e-REJECTED-canary-${runId}`
    const canaryTraceId = randomTraceId()
    await step('a batch sent once over quota is REJECTED with HTTP 413 (not silently accepted)', async () => {
      const payload = buildTraceExportRequest({
        resourceAttrs,
        spans: [{ traceId: canaryTraceId, spanId: randomSpanId(), name: canarySpanName }],
      })
      const res = await sendOtlp({ apiBaseUrl, apiKey: ingestKey, projectId: project.id, signal: 'traces', payload })
      if (res.status !== 413) {
        throw new Error(`expected HTTP 413 (QuotaExceeded) once over quota, got HTTP ${res.status}: ${res.bodyText}`)
      }
      if (!/quota/i.test(res.bodyText)) {
        throw new Error(`413 response body did not mention "quota": ${res.bodyText}`)
      }
    })

    await step('rejection persists on a second attempt (not a one-off)', async () => {
      const payload = buildTraceExportRequest({
        resourceAttrs,
        spans: [{ traceId: randomTraceId(), spanId: randomSpanId(), name: `${canarySpanName}-2` }],
      })
      const res = await sendOtlp({ apiBaseUrl, apiKey: ingestKey, projectId: project.id, signal: 'traces', payload })
      if (res.status !== 413) {
        throw new Error(`expected HTTP 413 again on the second over-quota attempt, got HTTP ${res.status}: ${res.bodyText}`)
      }
    })

    await step('the rejected canary span never actually landed (queried back, not just "the HTTP call errored")', async () => {
      const res = unwrap(
        await queryTraces({ client, query: { project_id: project.id, trace_id: canaryTraceId } }),
        'queryTraces',
      )
      if (res.data.length !== 0) {
        throw new Error(`rejected canary span WAS stored despite the 413: ${JSON.stringify(res.data)}`)
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')})`)
    } else {
      const td = await teardown(client, { projectIds })
      log(`\n▶ teardown: deleted ${td.deletedProjects} project(s)` + (td.errors.length ? ` (${td.errors.length} errors)` : ''))
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: OtelQuotaScenarioResult = { runId, ok, steps, quota: quotaSummary }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ otel-quota-scenario PASSED' : '❌ otel-quota-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
