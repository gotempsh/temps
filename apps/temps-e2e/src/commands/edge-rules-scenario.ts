// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Edge rules (ADR-089) end to end against a live Temps instance: rules are
 * managed with the REAL @temps-sdk/cli binary (see lib/cli-exec.ts), and
 * every claim about enforcement is checked with real HTTP requests through
 * the instance's proxy to a deployed app, never by reading the API back.
 *
 *   1. deploy a public image and wait until the proxy serves it
 *   2. `edge-rules create` a block rule without --mode: it must start
 *      log-only, so the app keeps answering /admin while the rule's match
 *      counter climbs (proving the proxy loaded and evaluated it)
 *   3. `edge-rules test`: the dry run reports the log-only match
 *   4. `edge-rules enforce`: /admin (any case) is now refused with the rule's
 *      status and body, while / and the ACME challenge path still pass
 *   5. `edge-rules apply` adds a redirect rule from a file; the plan, the
 *      live 308 with the query substituted, and a second apply that has
 *      nothing to do (apply is idempotent)
 *   6. `edge-rules delete` both rules: /admin reaches the app again
 *
 * Teardown deletes the deployment and project in a finally block, so a
 * failed step never leaks a live app.
 */
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { connect } from 'node:net'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { runCliJson, runCliOk, type CliConfig } from '../lib/cli-exec.ts'
import { makeClient, resolveConfig } from '../lib/client.ts'
import {
  assertNotConsoleFallback,
  createE2eProject,
  deployImage,
  fetchBody,
  getDeployStatus,
  getProductionEnvironment,
  looksLikeConsoleFallback,
  makeRunId,
  resolveLoadTarget,
  sleep,
  teardown,
  waitForDeployment,
  waitForHttpReady,
} from '../lib/flows.ts'

export interface EdgeRulesScenarioOptions {
  image?: string
  port?: string
  keep?: boolean
  deployTimeout?: string
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface EdgeRule {
  id: number
  name: string
  mode: 'enforce' | 'log_only'
  hits?: number
}

interface RuleList {
  rules: EdgeRule[]
}

interface Evaluation {
  decision: { kind: string; status?: number; rule_id?: number }
  matched_rule_id?: number | null
  matched_mode?: string | null
}

interface ApplyResult {
  plan: { create: unknown[]; update: unknown[]; remove: unknown[]; order: string[] }
  applied: boolean
}

const BLOCK_NAME = 'e2e block admin'
const REDIRECT_NAME = 'e2e redirect old'
const BLOCK_BODY = 'Blocked by an edge rule'

const blockExpression = {
  combinator: 'all',
  children: [
    { kind: 'condition', field: 'path', op: 'starts_with', values: ['/admin'], case_insensitive: true },
  ],
}

/** The proxy reloads rules every 5 s and flushes match counts every 10 s. */
const PROPAGATION_MS = 45_000

export async function edgeRulesScenarioCommand(opts: EdgeRulesScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const cli: CliConfig = { url: cfg.url, apiKey: cfg.apiKey }
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps edge rules scenario  ->  ${cfg.url}`)

  const runId = makeRunId(Date.now())
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

  const image = opts.image ?? 'traefik/whoami:latest'
  const port = Number(opts.port ?? '80')
  const projectIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []
  const scratch = await mkdtemp(path.join(tmpdir(), 'temps-edge-rules-e2e-'))

  try {
    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-edge-rules`, exposedPort: port }),
    )
    projectIds.push(project.id)
    const projectArgs = ['--project-id', String(project.id)]
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () =>
      getProductionEnvironment(client, project.id),
    )
    const deploymentId = await step('deploy image', () =>
      deployImage(client, { projectId: project.id, environmentId: env.id, imageRef: image }),
    )
    deployments.push({ projectId: project.id, deploymentId })
    await step('wait for deployment', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId,
        timeoutMs: Number(opts.deployTimeout ?? '300000'),
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )
    await step('confirm deployment did not fail', async () => {
      const s = await getDeployStatus(client, project.id, deploymentId)
      if (!s.ok) throw new Error(`deployment ${deploymentId} is in state "${s.state}"`)
    })

    if (!env.mainUrl) throw new Error('deployment produced no public URL to hit')
    const target = resolveLoadTarget(cfg.url, env.mainUrl)
    const origin = target.url.replace(/\/$/, '')
    const get = (p: string) => fetchBody(`${origin}${p}`, target.headers)
    await step('wait until the proxy serves the app', async () => {
      await waitForHttpReady({ url: `${origin}/`, headers: target.headers, timeoutMs: 120_000 })
      await assertNotConsoleFallback({ url: `${origin}/`, headers: target.headers })
    })

    const created = await step('edge-rules create (no --mode) starts log-only', async () => {
      const rule = await runCliJson<EdgeRule>(
        [
          'edge-rules', 'create', ...projectArgs,
          '--name', BLOCK_NAME,
          '--expression', JSON.stringify(blockExpression),
          '--action', JSON.stringify({ type: 'block', status: 403, body: BLOCK_BODY }),
        ],
        cli,
      )
      if (rule.mode !== 'log_only') {
        throw new Error(`a rule created without --mode must be log-only, got "${rule.mode}"`)
      }
      return rule
    })

    await step('log-only: the app still answers /admin and the proxy counts the match', async () => {
      const deadline = performance.now() + PROPAGATION_MS
      let hits = 0
      while (performance.now() < deadline) {
        const r = await get('/admin/users')
        if (r.status === 403 || r.body.includes(BLOCK_BODY)) {
          throw new Error(`a log-only rule acted on /admin/users (HTTP ${r.status})`)
        }
        if (r.status !== 200 || looksLikeConsoleFallback(r.body)) {
          throw new Error(`the app did not answer /admin/users (HTTP ${r.status})`)
        }
        const list = await runCliJson<RuleList>(['edge-rules', 'list', ...projectArgs], cli)
        hits = list.rules.find((rule) => rule.id === created.id)?.hits ?? 0
        if (hits > 0) return
        await sleep(2_000)
      }
      throw new Error(`the rule recorded no matches within ${PROPAGATION_MS / 1000}s; the proxy never evaluated it`)
    })

    await step('edge-rules test reports the log-only match without deciding', async () => {
      const result = await runCliJson<Evaluation>(
        ['edge-rules', 'test', ...projectArgs, '--url', `${env.mainUrl.replace(/\/$/, '')}/ADMIN`],
        cli,
      )
      if (result.decision.kind !== 'forward' || result.matched_rule_id !== created.id) {
        throw new Error(`expected a forwarded request matched by rule ${created.id}: ${JSON.stringify(result)}`)
      }
      if (result.matched_mode !== 'log_only') {
        throw new Error(`expected matched_mode log_only, got ${JSON.stringify(result.matched_mode)}`)
      }
    })

    await step('edge-rules enforce blocks /admin in any case, with the rule\'s body', async () => {
      const rule = await runCliJson<EdgeRule>(['edge-rules', 'enforce', String(created.id), ...projectArgs], cli)
      if (rule.mode !== 'enforce') throw new Error(`enforce left the rule in "${rule.mode}" mode`)
      const result = await runCliJson<Evaluation>(
        ['edge-rules', 'test', ...projectArgs, '--url', '/admin'],
        cli,
      )
      if (result.decision.kind !== 'block' || result.decision.status !== 403) {
        throw new Error(`the dry run should now block: ${JSON.stringify(result.decision)}`)
      }
      await waitForResponse(get, '/ADMIN/users', (r) => r.status === 403 && r.body.includes(BLOCK_BODY))
    })

    await step('enforced: unmatched paths and the ACME challenge path still pass', async () => {
      const home = await get('/')
      if (home.status !== 200 || looksLikeConsoleFallback(home.body)) {
        throw new Error(`/ should still reach the app, got HTTP ${home.status}`)
      }
      // Not a block: a rule must never be able to stop certificate renewal.
      const acme = await get('/.well-known/acme-challenge/e2e-probe')
      if (acme.status === 403 || acme.body.includes(BLOCK_BODY)) {
        throw new Error('the ACME challenge path was evaluated by edge rules')
      }
      // An exempt prefix cannot carry another path past the rules. Sent raw:
      // fetch() would resolve the dot segments before the proxy saw them.
      const originUrl = new URL(origin)
      if (originUrl.protocol === 'http:') {
        const status = await rawStatus(
          originUrl.hostname,
          Number(originUrl.port || 80),
          target.host,
          '/.well-known/acme-challenge/../../admin',
        )
        if (status !== 403) {
          throw new Error(`a dot-segment path under the ACME prefix skipped the rules (HTTP ${status})`)
        }
      } else {
        log('  (dot-segment check skipped: the proxy origin is not plain HTTP)')
      }
    })

    await step('edge-rules apply adds a redirect rule from a file', async () => {
      const exported = await runCliOk(['edge-rules', 'export', ...projectArgs], cli)
      const file = JSON.parse(exported.stdout) as { rules: Array<Record<string, unknown>> }
      file.rules.unshift({
        name: REDIRECT_NAME,
        mode: 'enforce',
        expression: {
          combinator: 'all',
          children: [{ kind: 'condition', field: 'path', op: 'eq', values: ['/old'] }],
        },
        action: { type: 'redirect', target: '/new?{query}', status: 308 },
      })
      const rulesFile = path.join(scratch, 'edge-rules.json')
      await writeFile(rulesFile, JSON.stringify(file, null, 2))

      const dryRun = await runCliJson<ApplyResult>(
        ['edge-rules', 'apply', ...projectArgs, '--file', rulesFile, '--dry-run'],
        cli,
      )
      if (dryRun.applied || dryRun.plan.create.length !== 1 || dryRun.plan.remove.length !== 0) {
        throw new Error(`expected a plan with one create: ${JSON.stringify(dryRun.plan)}`)
      }
      const applied = await runCliJson<ApplyResult>(
        ['edge-rules', 'apply', ...projectArgs, '--file', rulesFile, '--yes'],
        cli,
      )
      if (!applied.applied) throw new Error('apply reported nothing applied')

      const list = await runCliJson<RuleList>(['edge-rules', 'list', ...projectArgs], cli)
      const names = list.rules.map((r) => r.name)
      if (names.join('|') !== [REDIRECT_NAME, BLOCK_NAME].join('|')) {
        throw new Error(`rules are not in the file's order: ${JSON.stringify(names)}`)
      }
      if (list.rules.find((r) => r.name === BLOCK_NAME)?.id !== created.id) {
        throw new Error('apply replaced the existing rule instead of keeping its id')
      }

      const again = await runCliJson<ApplyResult>(
        ['edge-rules', 'apply', ...projectArgs, '--file', rulesFile, '--dry-run'],
        cli,
      )
      const pending = again.plan.create.length + again.plan.update.length + again.plan.remove.length
      if (again.applied || pending !== 0) {
        throw new Error(`re-applying the same file should change nothing: ${JSON.stringify(again.plan)}`)
      }
    })

    await step('the redirect is live, with the query carried over', async () => {
      const deadline = performance.now() + PROPAGATION_MS
      let last = ''
      while (performance.now() < deadline) {
        const res = await fetch(`${origin}/old?page=2`, { headers: target.headers, redirect: 'manual' })
        await res.arrayBuffer().catch(() => undefined)
        last = `HTTP ${res.status} location=${res.headers.get('location')}`
        if (res.status === 308 && res.headers.get('location') === '/new?page=2') return
        await sleep(2_000)
      }
      throw new Error(`/old?page=2 was not redirected to /new?page=2 (last: ${last})`)
    })

    await step('edge-rules delete removes both rules and /admin reaches the app again', async () => {
      const list = await runCliJson<RuleList>(['edge-rules', 'list', ...projectArgs], cli)
      for (const rule of list.rules) {
        await runCliOk(['edge-rules', 'delete', String(rule.id), ...projectArgs, '--yes'], cli)
      }
      const after = await runCliJson<RuleList>(['edge-rules', 'list', ...projectArgs], cli)
      if (after.rules.length !== 0) throw new Error(`${after.rules.length} rule(s) left after delete`)
      await waitForResponse(
        get,
        '/admin/users',
        (r) => r.status === 200 && !looksLikeConsoleFallback(r.body),
      )
    })
  } catch {
    // Recorded in `steps`; fall through to teardown.
  } finally {
    await rm(scratch, { recursive: true, force: true })
    if (opts.keep) {
      log(`\n(kept resources: projects=${projectIds.join(',')})`)
    } else {
      // Deleting the project removes its rules with it.
      const td = await teardown(client, { deployments, projectIds })
      log(
        `\n▶ teardown: tore down ${td.teardownDeployments} deployment(s), deleted ${td.deletedProjects} project(s)` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  if (json) {
    process.stdout.write(JSON.stringify({ runId, ok, steps }, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ edge-rules-scenario PASSED' : '❌ edge-rules-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}

/** The status of one HTTP/1.1 request sent byte for byte, path unnormalized. */
function rawStatus(hostname: string, port: number, host: string, requestPath: string): Promise<number> {
  return new Promise((resolve, reject) => {
    const socket = connect({ host: hostname, port })
    let head = ''
    const timer = setTimeout(() => {
      socket.destroy()
      reject(new Error(`no response to the raw request for ${requestPath} within 10s`))
    }, 10_000)
    socket.on('connect', () => {
      socket.write(`GET ${requestPath} HTTP/1.1\r\nHost: ${host}\r\nConnection: close\r\n\r\n`)
    })
    socket.on('data', (chunk: Buffer) => {
      head += chunk.toString('latin1')
      const line = head.split('\r\n', 1)[0] ?? ''
      if (head.includes('\r\n')) {
        clearTimeout(timer)
        socket.destroy()
        const status = Number(line.split(' ')[1])
        if (Number.isInteger(status)) resolve(status)
        else reject(new Error(`unexpected status line for ${requestPath}: ${JSON.stringify(line)}`))
      }
    })
    socket.on('error', (e) => {
      clearTimeout(timer)
      reject(e)
    })
  })
}

/** Polls one path until the proxy, having reloaded its rules, answers as expected. */
async function waitForResponse(
  get: (p: string) => Promise<{ status: number; body: string }>,
  p: string,
  done: (r: { status: number; body: string }) => boolean,
): Promise<void> {
  const deadline = performance.now() + PROPAGATION_MS
  let last = { status: 0, body: '' }
  while (performance.now() < deadline) {
    try {
      last = await get(p)
      if (done(last)) return
    } catch {
      // transient; retry
    }
    await sleep(2_000)
  }
  throw new Error(
    `${p} did not answer as expected within ${PROPAGATION_MS / 1000}s ` +
      `(last: HTTP ${last.status}, body starts ${JSON.stringify(last.body.slice(0, 80))})`,
  )
}
