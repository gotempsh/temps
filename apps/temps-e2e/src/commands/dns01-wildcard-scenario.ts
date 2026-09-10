// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Wildcard-certificate issuance via DNS-01 against a live Temps instance:
 *   1. register a Pebble-backed DNS provider and add + verify a managed zone
 *      for it (verification against Pebble always succeeds -- it's a mock)
 *   2. create a wildcard domain (`*.<zone>`) with a `dns-01` challenge, which
 *      auto-requests a real ACME order from Pebble
 *   3. push the order's TXT record(s) to the Pebble provider via `setup-dns`
 *      (this is what a real operator would do manually for a real provider;
 *      here it's `PebbleDnsProvider::create_record` POSTing to
 *      pebble-challtestsrv's `/set-txt`)
 *   4. finalize the order -- Pebble's REAL validator queries
 *      pebble-challtestsrv (its own `-dnsserver`) for the TXT record and only
 *      issues if it actually finds the value just pushed in step 3. This is
 *      the one thing `crates/temps-domains`' own `#[ignore]`d
 *      `test_dns01_wildcard_with_pebble` does NOT prove: that test sets
 *      `PEBBLE_VA_ALWAYS_VALID=1`, which skips real validation entirely, so
 *      nothing before this scenario proved the DNS-01 pipeline for real.
 *   5. parse the returned certificate PEM directly (no live app to fetch --
 *      a bare wildcard domain isn't routed to any project) and assert the
 *      issuer is Pebble's test root and the SAN covers the exact wildcard
 *   6. tear everything down (domain, managed zone, DNS provider) unless --keep
 *
 * Requires a target instance actually configured to talk to Pebble
 * (ACME_DIRECTORY_URL / ACME_INSECURE / TEMPS_ALLOW_PEBBLE_PROVIDER=1) -- see
 * apps/temps-e2e/README.md. Does NOT need the host-IP registration
 * `tls-scenario` needs for HTTP-01: DNS-01 validation never dials the host,
 * only pebble-challtestsrv's DNS interface, which is already wired to Pebble
 * via its `-dnsserver` flag.
 */
import { X509Certificate } from 'node:crypto'
import { makeClient, resolveConfig } from '../lib/client.ts'
import {
  makeRunId,
  setupPebbleDnsZone,
  setupDns01WildcardDomain,
  finalizeTlsCertificate,
  teardownDns01Resources,
} from '../lib/flows.ts'

export interface Dns01WildcardScenarioOptions {
  pebbleManagementUrl?: string
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

interface Dns01WildcardScenarioResult {
  runId: string
  ok: boolean
  wildcardDomain: string
  issuer: string
  subjectAltName: string
  steps: StepLog[]
}

export async function dns01WildcardScenarioCommand(opts: Dns01WildcardScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps DNS-01 wildcard-cert scenario  ->  ${cfg.url}`)

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

  const zone = `${runId}.e2e.test`
  const wildcardDomain = `*.${zone}`

  let dnsProviderId: number | undefined
  let issuer = ''
  let subjectAltName = ''

  try {
    const provider = await step('register Pebble DNS provider + verify managed zone', () =>
      setupPebbleDnsZone(client, { zone, managementUrl: opts.pebbleManagementUrl }),
    )
    dnsProviderId = provider.dnsProviderId
    log(`  dns-provider #${dnsProviderId}  zone=${zone}`)

    const setup = await step('create wildcard domain (dns-01) + push TXT record(s) to Pebble', () =>
      setupDns01WildcardDomain(client, { wildcardDomain, dnsProviderId: provider.dnsProviderId }),
    )
    log(`  domain #${setup.domainId}  txt records ${setup.recordsCreated}/${setup.totalRecords}`)

    await step('assert all required TXT records were created', async () => {
      if (setup.totalRecords === 0) {
        throw new Error('order carried zero DNS TXT records -- nothing to validate')
      }
      if (setup.recordsCreated !== setup.totalRecords) {
        throw new Error(`only ${setup.recordsCreated}/${setup.totalRecords} TXT record(s) were created`)
      }
    })

    const certificatePem = await step(
      'finalize order (real Pebble DNS-01 validation against pebble-challtestsrv)',
      async () => {
        const result = await finalizeTlsCertificate(client, setup.domainId)
        if (!result.ok) {
          throw new Error(result.reason)
        }
        return result.certificatePem
      },
    )

    await step('parse issued certificate and assert it covers the wildcard, issued by Pebble', async () => {
      const cert = new X509Certificate(certificatePem)
      issuer = cert.issuer
      subjectAltName = cert.subjectAltName ?? ''
      if (!/pebble/i.test(cert.issuer)) {
        throw new Error(`unexpected certificate issuer (expected Pebble's test root): ${cert.issuer}`)
      }
      const wantSan = `DNS:${wildcardDomain}`
      if (!subjectAltName.split(',').map((s) => s.trim()).includes(wantSan)) {
        throw new Error(`certificate SAN does not cover ${wantSan}: ${subjectAltName}`)
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: wildcardDomain=${wildcardDomain} dnsProviderId=${dnsProviderId ?? '-'})`)
    } else {
      const errors = await teardownDns01Resources(client, { wildcardDomain, dnsProviderId, zone })
      log(
        `\n▶ teardown: removed wildcard domain, managed zone, and DNS provider` +
          (errors.length ? ` (${errors.length} errors)` : ''),
      )
      for (const e of errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: Dns01WildcardScenarioResult = { runId, ok, wildcardDomain, issuer, subjectAltName, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ dns01-wildcard-scenario PASSED' : '❌ dns01-wildcard-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
