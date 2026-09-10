// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * End-to-end check for the proxy's `Accept: text/markdown` content negotiation
 * ("Markdown for Agents") against a real deployed app:
 *   1. create a project + deploy a real HTML-serving image
 *   2. wait until it's healthy and actually serving (not the console fallback)
 *   3. Accept: text/markdown            -> Content-Type: text/markdown, real
 *                                          converted Markdown body (not raw HTML)
 *   4. no Accept header                 -> Content-Type: text/html unchanged
 *   5. Accept: text/html,text/markdown;q=0.9 -> still converts (quality factors ignored)
 *   6. a 404 on the app with Accept: text/markdown -> stays text/html, untouched
 *      body (the gate must cancel conversion for non-2xx responses)
 *   7. tear everything down (unless --keep)
 *
 * Regression coverage: response_filter commits `Content-Type: text/markdown` to
 * the client before response_body_filter ever sees the body (Pingora sends
 * headers before streaming the body), so a bug in the body-conversion fallback
 * paths can leak raw, unconverted HTML under a `text/markdown` header. This
 * exercises that behavior against the real proxy + a real container, not just
 * the crate's unit tests.
 */
import { makeClient, resolveConfig, type TempsClientConfig } from '../lib/client.ts'
import type { Client } from '@temps-sdk/api/client'
import {
  createE2eProject,
  getProductionEnvironment,
  deployImage,
  waitForDeployment,
  waitForHttpReady,
  assertNotConsoleFallback,
  resolveLoadTarget,
  getDeployStatus,
  teardown,
  makeRunId,
} from '../lib/flows.ts'
import type { StepLog } from './scenario.ts'

export interface MarkdownOptions {
  image: string
  port?: string
  keep?: boolean
  deployTimeout?: string
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

export interface MarkdownResult {
  runId: string
  ok: boolean
  url: string
  appUrl: string
  steps: StepLog[]
}

async function fetchProbe(
  url: string,
  headers: Record<string, string>,
): Promise<{ status: number; contentType: string; body: string }> {
  const ctrl = new AbortController()
  const t = setTimeout(() => ctrl.abort(), 15_000)
  try {
    const res = await fetch(url, { headers, signal: ctrl.signal })
    const body = await res.text()
    return { status: res.status, contentType: res.headers.get('content-type') ?? '', body }
  } finally {
    clearTimeout(t)
  }
}

/**
 * Run the deploy -> markdown-negotiation-assertions -> teardown lifecycle and
 * return a structured result. Mirrors `runScenarioForImage`'s shape: never
 * throws for an expected failure — it's recorded in `steps` and reflected in
 * `ok`; only throws if teardown itself cannot run.
 */
export async function runMarkdownScenario(
  client: Client,
  cfg: TempsClientConfig,
  spec: { image: string; port: number; keep?: boolean; deployTimeoutMs?: number },
  log: (msg: string) => void,
): Promise<MarkdownResult> {
  const runId = makeRunId(Date.now())
  const steps: StepLog[] = []
  const projectIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []

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

  let appUrl = ''

  try {
    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-md`, exposedPort: spec.port }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () =>
      getProductionEnvironment(client, project.id),
    )
    appUrl = env.mainUrl
    log(`  env #${env.id} (${env.name})  url=${appUrl}`)

    const deploymentId = await step('deploy image', () =>
      deployImage(client, { projectId: project.id, environmentId: env.id, imageRef: spec.image }),
    )
    deployments.push({ projectId: project.id, deploymentId })
    log(`  deployment #${deploymentId}  image=${spec.image}`)

    await step('wait for deployment', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId,
        timeoutMs: spec.deployTimeoutMs ?? 300_000,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )

    await step('confirm deployment did not fail', async () => {
      const s = await getDeployStatus(client, project.id, deploymentId)
      if (!s.ok) {
        throw new Error(
          `deployment ${deploymentId} is in state "${s.state}" — the app did not start`,
        )
      }
    })

    if (!appUrl) throw new Error('deployment produced no public URL to hit')
    const target = resolveLoadTarget(cfg.url, appUrl)
    const rootUrl = target.url.replace(/\/$/, '') + '/'
    log(`  target ${rootUrl}  (Host: ${target.host})`)

    await step('wait for HTTP ready', () =>
      waitForHttpReady({
        url: rootUrl,
        headers: target.headers,
        timeoutMs: 120_000,
        onPoll: (status) => log(`    ...HTTP ${status || 'unreachable'}`),
      }),
    )

    await step('assert app is serving (not console fallback)', () =>
      assertNotConsoleFallback({ url: rootUrl, headers: target.headers }),
    )

    await step('Accept: text/markdown -> converted Markdown body', async () => {
      const r = await fetchProbe(rootUrl, { ...target.headers, Accept: 'text/markdown' })
      if (r.status !== 200) {
        throw new Error(`expected HTTP 200, got ${r.status}`)
      }
      if (!r.contentType.includes('text/markdown')) {
        throw new Error(`expected Content-Type: text/markdown, got "${r.contentType}"`)
      }
      // The exact bug this proves fixed: response_filter already committed
      // text/markdown to the client before the body was converted, and buggy
      // fallback paths used to leak the untouched HTML bytes under that header.
      // A real (even minimally) converted body must never start with an HTML
      // tag once the header says markdown.
      const trimmed = r.body.trimStart()
      if (trimmed.startsWith('<')) {
        throw new Error(
          `Content-Type says text/markdown but the body looks like raw HTML: ` +
            `${JSON.stringify(r.body.slice(0, 120))}`,
        )
      }
      if (r.body.trim().length === 0) {
        throw new Error('converted Markdown body is empty')
      }
      log(`  content-type: ${r.contentType}`)
      log(`  body starts: ${JSON.stringify(r.body.slice(0, 80))}`)
    })

    await step('no Accept header -> unchanged text/html', async () => {
      const r = await fetchProbe(rootUrl, { ...target.headers })
      if (r.status !== 200) {
        throw new Error(`expected HTTP 200, got ${r.status}`)
      }
      if (!r.contentType.includes('text/html')) {
        throw new Error(`expected Content-Type: text/html, got "${r.contentType}"`)
      }
      if (!r.body.trimStart().startsWith('<')) {
        throw new Error('expected raw HTML body when markdown was not requested')
      }
    })

    await step('Accept: text/html,text/markdown;q=0.9 -> still converts', async () => {
      const r = await fetchProbe(rootUrl, {
        ...target.headers,
        Accept: 'text/html,text/markdown;q=0.9',
      })
      if (!r.contentType.includes('text/markdown')) {
        throw new Error(
          `quality-factor Accept header should still trigger conversion, got Content-Type "${r.contentType}"`,
        )
      }
    })

    await step('404 + Accept: text/markdown -> stays text/html (gate cancels non-2xx)', async () => {
      const notFoundUrl = rootUrl.replace(/\/$/, '') + '/temps-e2e-markdown-404-probe'
      const r = await fetchProbe(notFoundUrl, { ...target.headers, Accept: 'text/markdown' })
      if (r.status < 400) {
        throw new Error(`expected a non-2xx status for a nonexistent path, got ${r.status}`)
      }
      if (r.contentType.includes('text/markdown')) {
        throw new Error(
          `a non-2xx response must never be converted — got Content-Type "${r.contentType}"`,
        )
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (spec.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')})`)
    } else {
      const td = await teardown(client, { deployments, projectIds })
      log(
        `\n▶ teardown: tore down ${td.teardownDeployments} deployment(s), ` +
          `deleted ${td.deletedProjects} project(s)` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.every((s) => s.ok)
  return { runId, ok, url: cfg.url, appUrl, steps }
}

export async function markdownCommand(opts: MarkdownOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }

  if (!json) log(`Temps e2e markdown-for-agents check  ->  ${cfg.url}`)

  const result = await runMarkdownScenario(
    client,
    cfg,
    {
      image: opts.image,
      port: Number(opts.port ?? '80'),
      keep: opts.keep,
      deployTimeoutMs: Number(opts.deployTimeout ?? '300000'),
    },
    log,
  )
  if (!json) process.stderr.write('\n')

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${result.ok ? '✅ markdown check PASSED' : '❌ markdown check FAILED'}`)
  }
  if (!result.ok) process.exitCode = 1
}
