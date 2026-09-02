// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! TLS certificate management for Traefik-discovered routes (ADR-041).
//!
//! Two operator-initiated paths to HTTPS for a host that Temps discovered from
//! Traefik labels but did not deploy:
//!
//!   Path A — request ACME issuance:
//!     temps traefik-discovery tls request <host> --challenge-type http-01
//!
//!   Path B — import Traefik's existing certificate:
//!     temps traefik-discovery tls import acme.json --hosts app.example.com
//!
//!   Deauthorize (stop renewal, do not delete cert):
//!     temps traefik-discovery tls revoke <host>
//!
//! These endpoints are implemented in crates/temps-deployments and are fully
//! documented in the generated SDK — see requestDiscoveredRouteCert,
//! deauthorizeDiscoveredRouteCert, and importTraefikAcmeJson below.

import { readFileSync } from 'node:fs'
import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  requestDiscoveredRouteCert,
  deauthorizeDiscoveredRouteCert,
  importTraefikAcmeJson,
} from '../../api/sdk.gen.js'
import type { ImportTraefikAcmeJsonResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, header, icons, json, colors, keyValue, success, warning, error as printError } from '../../ui/output.js'

// ── Pure helpers (unit tested) ────────────────────────────────────────────────

/** The only ACME/renewal method values the server accepts (ADR-041). */
export const VALID_ACME_METHODS = ['http-01', 'dns-01'] as const
export type AcmeMethod = (typeof VALID_ACME_METHODS)[number]

export function isValidChallengeType(value: string): value is AcmeMethod {
  return (VALID_ACME_METHODS as readonly string[]).includes(value)
}

export function isValidRenewalMethod(value: string): value is AcmeMethod {
  return (VALID_ACME_METHODS as readonly string[]).includes(value)
}

export type ReadAcmeJsonResult =
  | { ok: true; contents: string }
  | { ok: false; message: string }

/**
 * Read the acme.json file for the Path B import. Only reads bytes and never
 * parses/validates JSON here — the 8-step X.509 chain validation happens
 * server-side (ADR-041 §5) so the client can't diverge from it. Failures are
 * translated into a message an operator can act on instead of a raw
 * ENOENT/EISDIR stack.
 */
export function readAcmeJsonFile(file: string): ReadAcmeJsonResult {
  try {
    return { ok: true, contents: readFileSync(file, 'utf-8') }
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e)
    return { ok: false, message: `Failed to read '${file}': ${msg}` }
  }
}

// ── Commander registration ────────────────────────────────────────────────────

export function registerTraefikDiscoveryTlsCommands(parent: Command): void {
  const tls = parent
    .command('tls')
    .description(
      'Manage HTTPS certificates for Traefik-discovered routes (ADR-041). ' +
        'A discovered host has cert_eligible=false by design — no container label ever ' +
        'causes issuance. These commands let an operator explicitly authorize it.'
    )

  tls
    .command('request <host>')
    .description(
      'Authorize Temps to obtain a Let\'s Encrypt certificate for a discovered route ' +
        '(Path A). The certificate renews automatically using the declared challenge type.'
    )
    .option(
      '--challenge-type <type>',
      'Challenge type: http-01 (default) or dns-01',
      'http-01'
    )
    .option(
      '--acknowledge-manual-dns-renewal',
      'Confirm you accept manual DNS renewal when no auto-manage DNS zone is configured'
    )
    .option('--json', 'Output in JSON format')
    .action(requestCertAction)

  tls
    .command('revoke <host>')
    .description(
      'Remove TLS authorization for a discovered route. Stops Temps from attempting ' +
        'renewal. Does NOT delete the certificate — use `temps domains delete <host>` ' +
        'to remove the certificate itself.'
    )
    .option('--json', 'Output in JSON format')
    .action(revokeCertAction)

  tls
    .command('import <acme-json-file>')
    .description(
      'Import certificates from a Traefik acme.json file (Path B). ' +
        'Use this to get HTTPS immediately at cutover — Traefik already holds the cert, ' +
        'so there is no outage window. Each host is validated (8-step X.509 chain) and ' +
        'a per-host result is returned. Add --dry-run to preview without writing.'
    )
    .requiredOption(
      '--hosts <hosts>',
      'Comma-separated list of hostnames to import',
      (val: string) => val.split(',').map((h) => h.trim())
    )
    .option(
      '--renewal-method <method>',
      'How Temps will renew when the imported cert expires: http-01 (default) or dns-01',
      'http-01'
    )
    .option(
      '--acknowledge-manual-dns-renewal',
      'Confirm you accept manual DNS renewal when no auto-manage DNS zone is configured'
    )
    .option('--dry-run', 'Validate and preview; do not write any certificate')
    .option('--json', 'Output in JSON format')
    .action(importAcmeJsonAction)
}

// ── Actions ──────────────────────────────────────────────────────────────────

async function requestCertAction(
  host: string,
  options: {
    challengeType?: string
    acknowledgeManualDnsRenewal?: boolean
    json?: boolean
  }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const challengeType = options.challengeType ?? 'http-01'
  if (!isValidChallengeType(challengeType)) {
    printError(`Invalid --challenge-type '${challengeType}'. Must be 'http-01' or 'dns-01'.`)
    process.exit(1)
  }

  await withSpinner(`Requesting ${challengeType} certificate for ${host}...`, async () => {
    const { error } = await requestDiscoveredRouteCert({
      client,
      path: { host },
      body: {
        challenge_type: challengeType,
        acknowledge_manual_dns_renewal: options.acknowledgeManualDnsRenewal ?? false,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  if (options.json) {
    json({ host, challenge_type: challengeType, status: 'authorized' })
    return
  }

  newline()
  success(`Certificate authorization recorded for ${colors.bold(host)}`)
  newline()
  console.log(colors.muted(`  Challenge type : ${challengeType}`))
  console.log(
    colors.muted(
      `  The ACME challenge is now in flight. Use 'temps domains list' to monitor issuance.`
    )
  )
  if (challengeType === 'dns-01') {
    warning(
      'DNS-01 renewal requires a verified auto-manage DNS zone covering this host. ' +
        'If you do not have one, renewal will require a manual DNS update.'
    )
  }
  newline()
}

async function revokeCertAction(
  host: string,
  options: { json?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  await withSpinner(`Removing TLS authorization for ${host}...`, async () => {
    const { error } = await deauthorizeDiscoveredRouteCert({
      client,
      path: { host },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  if (options.json) {
    json({ host, status: 'deauthorized' })
    return
  }

  newline()
  success(`TLS authorization removed for ${colors.bold(host)}`)
  newline()
  console.log(
    colors.muted(
      '  Temps will no longer renew the certificate. The existing cert in the domains table ' +
        `is still there — use 'temps domains delete ${host}' to remove it.`
    )
  )
  newline()
}

async function importAcmeJsonAction(
  acmeJsonFile: string,
  options: {
    hosts: string[]
    renewalMethod?: string
    acknowledgeManualDnsRenewal?: boolean
    dryRun?: boolean
    json?: boolean
  }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const renewalMethod = options.renewalMethod ?? 'http-01'
  if (!isValidRenewalMethod(renewalMethod)) {
    printError(`Invalid --renewal-method '${renewalMethod}'. Must be 'http-01' or 'dns-01'.`)
    process.exit(1)
  }

  const read = readAcmeJsonFile(acmeJsonFile)
  if (!read.ok) {
    printError(read.message)
    process.exit(1)
  }
  const acmeJson = read.contents

  const label = options.dryRun
    ? `Validating ${options.hosts.length} host(s) from ${acmeJsonFile} (dry run)...`
    : `Importing ${options.hosts.length} host(s) from ${acmeJsonFile}...`

  const result = await withSpinner<ImportTraefikAcmeJsonResponse>(label, async () => {
    const { data, error } = await importTraefikAcmeJson({
      client,
      body: {
        acme_json: acmeJson,
        hosts: options.hosts,
        renewal_method: renewalMethod,
        acknowledge_manual_dns_renewal: options.acknowledgeManualDnsRenewal ?? false,
        dry_run: options.dryRun ?? false,
      },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(
    `${icons.lock} Import results${result.dry_run ? ' (dry run — nothing written)' : ''}`
  )
  newline()
  keyValue(
    'Summary',
    `${result.succeeded} succeeded, ${result.failed} failed of ${result.total_requested} requested`
  )
  newline()

  for (const verdict of result.verdicts) {
    if (verdict.success) {
      const expiry = verdict.not_after
        ? ` — expires ${new Date(verdict.not_after).toLocaleDateString()}`
        : ''
      console.log(`  ${colors.success('✓')} ${colors.bold(verdict.host)}${expiry}`)
      if (verdict.sans.length > 1) {
        console.log(colors.muted(`    SANs: ${verdict.sans.join(', ')}`))
      }
    } else {
      console.log(`  ${colors.error('✗')} ${colors.bold(verdict.host)}`)
      console.log(colors.muted(`    ${verdict.error ?? 'unknown error'}`))
    }
  }

  newline()

  if (result.dry_run && result.succeeded > 0) {
    console.log(colors.muted('  Re-run without --dry-run to import the above certificates.'))
    newline()
  }

  if (renewalMethod === 'dns-01' && result.succeeded > 0 && !options.acknowledgeManualDnsRenewal) {
    warning(
      'Imported certs with dns-01 renewal method. Verify you have a verified auto-manage ' +
        'DNS zone for each imported host, or renewal will require manual DNS updates.'
    )
    newline()
  }
}
