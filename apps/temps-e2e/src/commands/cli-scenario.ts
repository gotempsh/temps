/**
 * Genuine CLI end-to-end test: every step below spawns the REAL
 * @temps-sdk/cli binary as a subprocess (see lib/cli-exec.ts) against a live
 * Temps instance, using the exact commands/flags a human or AI agent would
 * type. This is deliberately different from scenario.ts/tls-scenario.ts/
 * email-scenario.ts, which drive the SDK client directly -- fast and useful
 * for testing the API, but they never prove argv parsing, Commander's
 * command wiring, or stdout formatting actually work, which is exactly what
 * breaks an agent running `bunx @temps-sdk/cli ...` even when the API is
 * fine.
 *
 * Covers the flows an agent actually chains in practice: create a project,
 * set/read/delete an env var, provision/inspect/remove a service, deploy a
 * public image and poll it to a terminal state via the CLI's own `deployments
 * status`, CRUD a domain record, and confirm the apikeys read paths work
 * under API-key auth (see the "known gaps" note below for why this doesn't
 * also mint a new key).
 *
 * Every created resource is torn down via the CLI itself in a finally block
 * (dogfooding delete/remove the same way create was dogfooded); if a CLI
 * teardown call itself fails, falls back to the SDK-based teardown() from
 * flows.ts so a broken `delete` command can't leak live resources.
 *
 * Bugs found live while building this:
 *  - FIXED: `environments vars get` used to read from the list endpoint,
 *    which the server masks to the literal string "***" for EVERY variable
 *    regardless of `is_secret` -- so it never actually showed a plain
 *    variable's value despite that being the entire point of the command.
 *    Fixed in apps/temps-cli/src/commands/environments/index.ts to resolve
 *    real values through the same audited per-key endpoint `vars export`
 *    already used (`getEnvironmentVariableValue`,
 *    `GET /projects/{id}/env-vars/{key}/value`). Secrets are still
 *    correctly withheld -- see the "environments vars get" step below,
 *    which now asserts on the real revealed value for a non-secret var.
 *  - NOT FIXED (out of scope -- needs tracing the certificate-listing
 *    query, not a CLI change): `domains status` 404s on a domain that
 *    hasn't finished ACME provisioning yet (the state every domain is in
 *    immediately after `add`) -- exactly the state you'd actually want to
 *    check status on. See the "domains status" note below.
 */
import { runCliOk, runCliJson, type CliConfig } from '../lib/cli-exec.ts'
import { makeClient, resolveConfig } from '../lib/client.ts'
import { makeRunId, isTerminalDeployStatus, teardown as sdkTeardown } from '../lib/flows.ts'

export interface CliScenarioOptions {
  image?: string
  keep?: boolean
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface CliScenarioResult {
  runId: string
  ok: boolean
  projectSlug: string
  steps: StepLog[]
}

/** Pull the array out of a `--json` list response regardless of the specific wrapper shape. */
function asArray(data: unknown, ...keys: string[]): Record<string, unknown>[] {
  if (Array.isArray(data)) return data as Record<string, unknown>[]
  if (data && typeof data === 'object') {
    for (const k of keys) {
      const v = (data as Record<string, unknown>)[k]
      if (Array.isArray(v)) return v as Record<string, unknown>[]
    }
  }
  throw new Error(`expected an array (checked keys: ${keys.join(', ')}), got: ${JSON.stringify(data).slice(0, 200)}`)
}

export async function cliScenarioCommand(opts: CliScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const cliCfg: CliConfig = { url: cfg.url, apiKey: cfg.apiKey }
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps CLI e2e scenario (real subprocess)  ->  ${cfg.url}`)

  const runId = makeRunId(Date.now())
  const image = opts.image ?? 'traefik/whoami:latest'
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

  const projectName = `${runId}-cli-app`
  const envVarKey = `E2E_VAR_${runId.toUpperCase().replace(/-/g, '_')}`
  const envVarValue = `value-${runId}`
  const serviceName = `${runId}-db`
  const domain = `${runId}.e2e.test`
  let projectSlug = ''
  let projectId: number | null = null
  let serviceId: number | null = null
  let domainCreated = false

  try {
    await step('projects create (non-interactive, manual/docker source)', () =>
      runCliOk(['projects', 'create', '--manual', '--name', projectName, '--port', '80', '-y'], cliCfg),
    )

    projectSlug = await step('resolve created project via `projects list --json`', async () => {
      const list = asArray(await runCliJson(['projects', 'list'], cliCfg), 'projects', 'data', 'items')
      const found = list.find((p) => p.name === projectName)
      if (!found) throw new Error(`project "${projectName}" not found in \`projects list --json\` output`)
      return String(found.slug)
    })
    log(`  project slug=${projectSlug}`)

    await step('projects show --json reflects the created project', async () => {
      const detail = (await runCliJson(['projects', 'show', '-p', projectSlug], cliCfg)) as Record<string, unknown>
      if (detail.name !== projectName) {
        throw new Error(`expected name "${projectName}", got ${JSON.stringify(detail.name)}`)
      }
      projectId = Number(detail.id)
    })

    await step('environments list --json shows a production environment', async () => {
      const list = asArray(
        await runCliJson(['environments', 'list', '-p', projectSlug], cliCfg),
        'environments',
        'data',
        'items',
      )
      const prod = list.find((e) => e.name === 'production')
      if (!prod) throw new Error(`no "production" environment in ${JSON.stringify(list)}`)
    })

    await step('environments vars set writes an env var', () =>
      runCliOk(
        ['environments', 'vars', 'set', envVarKey, envVarValue, '-p', projectSlug, '-e', 'production'],
        cliCfg,
      ),
    )

    await step('environments vars list --json shows the var', async () => {
      const list = asArray(
        await runCliJson(['environments', 'vars', 'list', '-p', projectSlug, '-e', 'production'], cliCfg),
        'variables',
        'data',
        'items',
      )
      const found = list.find((v) => v.key === envVarKey)
      if (!found) throw new Error(`"${envVarKey}" not found in vars list: ${JSON.stringify(list)}`)
    })

    // `vars get` used to always print "***" for every variable regardless of
    // is_secret (it read from the list endpoint, which deliberately masks
    // bulk reads) -- fixed to resolve real values through the same audited
    // per-key endpoint `vars export` uses. This asserts the fix directly.
    await step('environments vars get reveals the real value (regression test for the fix)', async () => {
      const result = await runCliOk(
        ['environments', 'vars', 'get', envVarKey, '-p', projectSlug, '-e', 'production'],
        cliCfg,
      )
      if (!result.stdout.includes(envVarValue)) {
        throw new Error(`expected stdout to contain "${envVarValue}", got: ${result.stdout}`)
      }
      if (result.stdout.includes('***')) {
        throw new Error(`"vars get" printed the masked placeholder instead of the real value: ${result.stdout}`)
      }
    })

    await step('environments vars export resolves the real value', async () => {
      const result = await runCliOk(
        ['environments', 'vars', 'export', '-p', projectSlug, '-e', 'production'],
        cliCfg,
      )
      if (!result.stdout.includes(`${envVarKey}=${envVarValue}`)) {
        throw new Error(`expected stdout to contain "${envVarKey}=${envVarValue}", got: ${result.stdout}`)
      }
    })

    await step('environments vars delete removes it', () =>
      runCliOk(['environments', 'vars', 'delete', envVarKey, '-p', projectSlug, '-e', 'production', '-f'], cliCfg),
    )

    await step('services create provisions a postgres service (non-interactive defaults)', () =>
      runCliOk(['services', 'create', '-t', 'postgres', '-n', serviceName, '-y'], cliCfg, { timeoutMs: 60_000 }),
    )

    serviceId = await step('resolve created service via `services list --json`', async () => {
      const list = asArray(await runCliJson(['services', 'list'], cliCfg), 'services', 'data', 'items')
      const found = list.find((s) => s.name === serviceName)
      if (!found) throw new Error(`service "${serviceName}" not found in \`services list --json\` output`)
      return Number(found.id)
    })
    log(`  service id=${serviceId}`)

    await step('services show --json reflects the created service', async () => {
      const detail = (await runCliJson(['services', 'show', '--id', String(serviceId)], cliCfg)) as {
        service?: { service_type?: string; status?: string }
      }
      if (detail.service?.service_type !== 'postgres') {
        throw new Error(`expected service_type postgres, got: ${JSON.stringify(detail)}`)
      }
    })

    await step('deploy:image starts a real deployment (--no-wait)', () =>
      runCliOk(
        ['deploy:image', '--image', image, '-p', projectSlug, '-e', 'production', '--no-wait', '-y'],
        cliCfg,
        { timeoutMs: 60_000 },
      ),
    )

    const deploymentId = await step('resolve the started deployment via `deployments list --json`', async () => {
      const list = asArray(
        await runCliJson(['deployments', 'list', '-p', projectSlug], cliCfg),
        'deployments',
        'data',
        'items',
      )
      if (list.length === 0) throw new Error('deployments list is empty right after deploy:image')
      // Freshest project, single deploy so far -- highest id is the one just started.
      const latest = list.reduce((a, b) => (Number(a.id) > Number(b.id) ? a : b))
      return Number(latest.id)
    })
    log(`  deployment id=${deploymentId}`)

    await step('poll `deployments status --json` to a terminal state via the real CLI', async () => {
      const timeoutMs = 120_000
      const start = performance.now()
      let last: Record<string, unknown> | null = null
      while (performance.now() - start < timeoutMs) {
        last = (await runCliJson(
          ['deployments', 'status', '-p', projectSlug, '-d', String(deploymentId)],
          cliCfg,
        )) as Record<string, unknown>
        const status = String(last.status ?? last.state ?? 'unknown')
        const { terminal, ok } = isTerminalDeployStatus(status)
        log(`    ...${status}`)
        if (terminal) {
          if (!ok) throw new Error(`deployment ${deploymentId} reached terminal state "${status}" (not success)`)
          return
        }
        await new Promise((r) => setTimeout(r, 3000))
      }
      throw new Error(
        `deployment ${deploymentId} did not reach a terminal state within ${timeoutMs / 1000}s (last: ${JSON.stringify(last)})`,
      )
    })

    await step('domains add creates a domain record', () =>
      runCliOk(['domains', 'add', '-d', domain, '-c', 'http-01', '-y'], cliCfg),
    )
    domainCreated = true

    await step('domains list --json shows the created domain', async () => {
      const list = asArray(await runCliJson(['domains', 'list'], cliCfg), 'domains', 'data', 'items')
      const found = list.find((d) => d.domain === domain)
      if (!found) throw new Error(`"${domain}" not found in domains list: ${JSON.stringify(list)}`)
    })

    // NOT `domains status` here -- discovered live: `check_domain_status`
    // (crates/temps-domains/src/handlers/domain_handler.rs) resolves via
    // `list_certificates(CertificateFilter::default())`, which only returns
    // domains that already have an issued certificate row. A domain that's
    // still `challenge_requested` (the state every domain is in immediately
    // after `add`, before an ACME order finalizes) has no certificate row
    // yet, so the ONE endpoint whose entire purpose is checking on a
    // still-provisioning domain 404s on it -- confirmed directly against the
    // API: `GET /domains/17/status` -> 404 for domain id 17, which
    // `GET /domains` in the same instant confirms exists. This is a real,
    // pre-existing backend bug, not a test bug -- out of scope to fix here
    // (needs tracing the certificate-listing query, not a CLI change).

    // NOT `apikeys create` here -- discovered live: it's correctly REJECTED
    // when authenticated via an API key ("This authentication method cannot
    // perform the requested sensitive action", crates/temps-auth/src/
    // sensitive_action.rs), a deliberate anti-privilege-escalation guard so a
    // leaked/scoped key can never mint itself a broader one (matches the
    // project's api-key privilege-escalation fix). This is correct product
    // behavior, not a bug -- but it does mean `apikeys create` is
    // structurally untestable from a pure API-key-driven CLI e2e run, since
    // API-key auth is this CLI's only non-interactive auth mode (no
    // username/password login path exists). Exercise the read paths that
    // ARE reachable under API-key auth instead.
    await step('apikeys list --json is reachable under API-key auth', async () => {
      const list = asArray(await runCliJson(['apikeys', 'list'], cliCfg), 'api_keys', 'keys', 'data', 'items')
      if (list.length === 0) throw new Error('expected at least the currently-authenticating key to be listed')
    })

    await step('apikeys permissions --json is reachable under API-key auth', () =>
      runCliOk(['apikeys', 'permissions', '--json'], cliCfg),
    )
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      const kept = [
        projectSlug && `project=${projectSlug}`,
        serviceId !== null && `service=${serviceId}`,
        domainCreated && `domain=${domain}`,
      ].filter(Boolean)
      log(`\n(kept resources that were actually created: ${kept.length ? kept.join(' ') : '(none got created)'})`)
    } else {
      const cliErrors: string[] = []
      let projectDeleteFailed = false
      let serviceRemoveFailed = false
      const cliTeardown = async (label: string, args: string[]): Promise<boolean> => {
        try {
          await runCliOk(args, cliCfg, { timeoutMs: 30_000 })
          return true
        } catch (e) {
          cliErrors.push(`${label}: ${(e as Error).message}`)
          return false
        }
      }
      if (domainCreated) await cliTeardown('domains remove', ['domains', 'remove', '-d', domain, '-f', '-y'])
      if (serviceId !== null) {
        serviceRemoveFailed = !(await cliTeardown('services remove', ['services', 'remove', '--id', String(serviceId), '-f', '-y']))
      }
      if (projectSlug) {
        projectDeleteFailed = !(await cliTeardown('projects delete', ['projects', 'delete', '-p', projectSlug, '-f', '-y']))
      }

      log(`\n▶ teardown (via CLI): ${cliErrors.length === 0 ? 'clean' : `${cliErrors.length} error(s)`}`)
      for (const e of cliErrors) log(`    ! ${e}`)

      // A broken `delete`/`remove` command shouldn't leak live resources just
      // because this is a test of that same command -- fall back to the
      // SDK-based teardown() (proven independent of the CLI) for exactly the
      // resources the CLI itself failed to remove.
      if (projectDeleteFailed || serviceRemoveFailed) {
        const fallback = await sdkTeardown(client, {
          deployments: [],
          projectIds: projectDeleteFailed && projectId !== null ? [projectId] : [],
          serviceIds: serviceRemoveFailed && serviceId !== null ? [serviceId] : [],
        })
        if (fallback.errors.length) {
          log(`    ! SDK fallback teardown also had errors: ${fallback.errors.join('; ')}`)
        } else {
          log('    SDK fallback teardown covered the CLI teardown gap')
        }
      }
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: CliScenarioResult = { runId, ok, projectSlug, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ cli-scenario PASSED' : '❌ cli-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
