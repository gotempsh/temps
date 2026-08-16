/**
 * TLS-via-Pebble lifecycle against a live Temps instance:
 *   1. deploy an app the normal way (reuses the `scenario` deploy path)
 *   2. register the host's IP with pebble-challtestsrv so Pebble's HTTP-01
 *      validation request actually reaches this machine
 *   3. create a throwaway custom domain + a standalone TLS-certificate
 *      record for it, linked together
 *   4. finalize the ACME order (real HTTP-01 exchange against Pebble --
 *      `createDomain` already auto-requested the challenge, so this is the
 *      completion step, not `POST /domains/{domain}/provision`; see
 *      `finalizeTlsCertificate` in flows.ts for why)
 *   5. dial the instance's TLS listener directly (SNI = the test domain) and
 *      assert BOTH that the real app answered (not the console fallback)
 *      AND that the served certificate's issuer is Pebble's test root, not
 *      a real CA
 *   6. tear everything down (domain, custom-domain route, deployment,
 *      project) unless --keep
 *
 * Requires a target instance that is actually configured to talk to Pebble
 * (ACME_DIRECTORY_URL / ACME_INSECURE) and whose plain-HTTP proxy listener is
 * reachable on Pebble's fixed httpPort (5002) — see apps/temps-e2e/README.md.
 */
import { makeClient, resolveConfig } from '../lib/client.ts'
import {
  createE2eProject,
  getProductionEnvironment,
  deployImage,
  waitForDeployment,
  getDeployStatus,
  teardown,
  makeRunId,
  registerPebbleDefaultIp,
  setupTlsDomain,
  finalizeTlsCertificate,
  teardownTlsDomain,
  waitForTlsAppReady,
} from '../lib/flows.ts'

export interface TlsScenarioOptions {
  image: string
  port?: string
  tlsPort?: string
  tlsHost?: string
  pebbleNetwork?: string
  pebbleManagementUrl?: string
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

interface TlsScenarioResult {
  runId: string
  ok: boolean
  domain: string
  issuer: Record<string, string>
  steps: StepLog[]
}

export async function tlsScenarioCommand(opts: TlsScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps TLS-via-Pebble scenario  ->  ${cfg.url}`)

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

  const domain = `tls-${runId}.e2e.test`
  const tlsPort = Number(opts.tlsPort ?? '8443')
  const tlsHost = opts.tlsHost ?? '127.0.0.1'
  const port = Number(opts.port ?? '80')
  const deployTimeoutMs = Number(opts.deployTimeout ?? '300000')

  const projectIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []
  let customDomainId: number | undefined
  let issuer: Record<string, string> = {}

  try {
    const project = await step('create project', () =>
      createE2eProject(client, { name: `${runId}-tls-app`, exposedPort: port }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    const env = await step('resolve production environment', () =>
      getProductionEnvironment(client, project.id),
    )
    log(`  env #${env.id} (${env.name})`)

    const deploymentId = await step('deploy image', () =>
      deployImage(client, { projectId: project.id, environmentId: env.id, imageRef: opts.image }),
    )
    deployments.push({ projectId: project.id, deploymentId })
    log(`  deployment #${deploymentId}  image=${opts.image}`)

    await step('wait for deployment', () =>
      waitForDeployment(client, {
        projectId: project.id,
        deploymentId,
        timeoutMs: deployTimeoutMs,
        onPoll: (s) => log(`    ...${s.state}`),
      }),
    )

    await step('confirm deployment did not fail', async () => {
      const s = await getDeployStatus(client, project.id, deploymentId)
      if (!s.ok) {
        throw new Error(`deployment ${deploymentId} is in state "${s.state}" — the app did not start`)
      }
    })

    const hostIp = await step('register host IP with pebble-challtestsrv', () =>
      registerPebbleDefaultIp({
        network: opts.pebbleNetwork,
        managementUrl: opts.pebbleManagementUrl,
      }),
    )
    log(`  pebble will resolve unknown hosts (incl. ${domain}) to ${hostIp}`)

    const tlsDomain = await step('create custom domain + TLS certificate record', () =>
      setupTlsDomain(client, { domain, projectId: project.id, environmentId: env.id }),
    )
    customDomainId = tlsDomain.customDomainId
    log(`  certificate #${tlsDomain.certificateId}  custom-domain #${tlsDomain.customDomainId}`)

    await step('finalize order (real ACME HTTP-01 exchange with Pebble)', async () => {
      const result = await finalizeTlsCertificate(client, tlsDomain.certificateId)
      if (!result.ok) {
        throw new Error(result.reason)
      }
    })

    const served = await step(
      'fetch app over HTTPS (waiting for route-table propagation) and inspect served certificate',
      () => waitForTlsAppReady({ host: tlsHost, port: tlsPort, sni: domain, path: '/' }),
    )
    issuer = served.issuer

    await step('assert certificate was issued by Pebble, not a real CA', async () => {
      const issuerStr = JSON.stringify(served.issuer)
      if (!/pebble/i.test(issuerStr)) {
        throw new Error(`unexpected certificate issuer (expected Pebble's test root): ${issuerStr}`)
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: domain=${domain} projects=${projectIds.join(',')})`)
    } else {
      const tlsErrors = await teardownTlsDomain(client, { projectId: projectIds[0] ?? 0, customDomainId, domain })
      const td = await teardown(client, { deployments, projectIds })
      log(
        `\n▶ teardown: tore down ${td.teardownDeployments} deployment(s), ` +
          `deleted ${td.deletedProjects} project(s), removed TLS domain` +
          (td.errors.length || tlsErrors.length ? ` (${td.errors.length + tlsErrors.length} errors)` : ''),
      )
      for (const e of [...tlsErrors, ...td.errors]) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: TlsScenarioResult = { runId, ok, domain, issuer, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ tls-scenario PASSED' : '❌ tls-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
