// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { execFileSync } from 'node:child_process'
import { X509Certificate } from 'node:crypto'
import { expect, expectAppMounted, test, uniqueSlug } from '../fixtures'

/**
 * Drives the actual Temps console UI through the 3-step "Add Domain" wizard,
 * completes a real ACME HTTP-01 exchange against Pebble (Let's Encrypt's own
 * ACME v2 test server), and confirms the console shows a genuine
 * Pebble-issued certificate -- not just that the API calls involved returned
 * 200.
 *
 * Requires the target instance to be configured for Pebble
 * (ACME_DIRECTORY_URL / ACME_INSECURE, see apps/temps-e2e/README.md) and its
 * plain-HTTP proxy listener to be reachable on Pebble's fixed httpPort
 * (5002), since Pebble dials the domain directly to serve the challenge --
 * this is not achievable against a normal dev slot.
 *
 * Certificate <-> project attachment (the piece that would make a deployed
 * app actually serve over this domain) has no console UI today -- only a raw
 * API (`POST /projects/{id}/custom-domains/{domain_id}/link-certificate/{cert_id}`).
 * This spec covers what the UI actually offers: provisioning a standalone
 * TLS certificate end to end.
 */

/**
 * Registers this host's IP with pebble-challtestsrv's mock-DNS management API
 * so Pebble's HTTP-01 validation request (which it sends to whatever the test
 * domain resolves to) actually reaches wherever this instance's proxy is
 * listening. Idempotent -- safe to call every run.
 *
 * Pebble and pebble-challtestsrv run in Docker while `temps serve` runs
 * natively on the host, so `host.docker.internal` has to be resolved from
 * INSIDE a container on that compose network; the host process itself can't
 * resolve that name.
 *
 * Docker Desktop (macOS/Windows) resolves `host.docker.internal` inside any
 * container for free, with a real IPv4 address. Linux Docker Engine (what CI
 * runners use) has no such magic and needs an explicit
 * `--add-host=host.docker.internal:host-gateway` -- but that same flag, when
 * added on Docker Desktop, resolves to an IPv6 host-gateway address instead
 * of reusing the free IPv4 resolution. So: try the free resolution first
 * (covers macOS), and only fall back to `--add-host` if that fails (covers
 * Linux CI) -- and always pick the IPv4-shaped address out of the result,
 * since `getent hosts` can return more than one line/family.
 */
function resolveHostDockerInternal(): string {
  const pickIpv4 = (out: string): string | undefined =>
    out
      .split('\n')
      .map((line) => line.trim().split(/\s+/)[0])
      .find((token) => /^\d{1,3}(\.\d{1,3}){3}$/.test(token ?? ''))

  const baseArgs = [
    'run',
    '--rm',
    '--network',
    'temps-e2e-pebble-net',
  ]
  const getentArgs = ['alpine:latest', 'getent', 'hosts', 'host.docker.internal']

  try {
    const out = execFileSync('docker', [...baseArgs, ...getentArgs], {
      encoding: 'utf8',
    })
    const ip = pickIpv4(out)
    if (ip) return ip
  } catch {
    // Fall through to the --add-host retry below (expected on Linux CI).
  }

  const out = execFileSync(
    'docker',
    [...baseArgs, '--add-host', 'host.docker.internal:host-gateway', ...getentArgs],
    { encoding: 'utf8' }
  )
  const ip = pickIpv4(out)
  if (!ip)
    throw new Error(`could not resolve an IPv4 host.docker.internal: ${JSON.stringify(out)}`)
  return ip
}

async function registerPebbleDefaultIp(): Promise<void> {
  const ip = resolveHostDockerInternal()

  const v4 = await fetch('http://localhost:8056/set-default-ipv4', {
    method: 'POST',
    body: JSON.stringify({ ip }),
  })
  if (!v4.ok)
    throw new Error(
      `pebble-challtestsrv /set-default-ipv4 failed: HTTP ${v4.status}`
    )

  // challtestsrv answers AAAA queries with `::1` unless cleared, and Go's
  // dialer (Pebble's own HTTP client) prefers a AAAA result when one exists --
  // so validation dials [::1]:5002 inside Pebble's own container (nothing
  // listens there) instead of the host unless this is cleared.
  const v6 = await fetch('http://localhost:8056/set-default-ipv6', {
    method: 'POST',
    body: JSON.stringify({ ip: '' }),
  })
  if (!v6.ok)
    throw new Error(
      `pebble-challtestsrv /set-default-ipv6 (clear) failed: HTTP ${v6.status}`
    )
}

test.describe('TLS certificate provisioning via Pebble (HTTP-01)', () => {
  test.beforeAll(async () => {
    await registerPebbleDefaultIp()
  })

  test('creates a domain through the wizard and completes real ACME HTTP-01 issuance', async ({
    page,
  }, testInfo) => {
    const domain = `${uniqueSlug('tls', testInfo)}.e2e.test`
    let domainId: number | null = null

    try {
      await page.goto('/domains/add')
      await expectAppMounted(page)

      // Step 1: Domain
      await page.getByLabel('Domain Name').fill(domain)
      await page.getByRole('button', { name: 'Next' }).click()

      // Step 2: Challenge type -- HTTP-01 is the default selection.
      await page.getByRole('button', { name: 'Next' }).click()

      // Step 3: Confirm + submit.
      await page
        .getByRole('button', { name: 'Create Domain & Start Provisioning' })
        .click()

      await page.waitForURL(/\/domains\/\d+/)
      const idMatch = page.url().match(/\/domains\/(\d+)/)
      expect(
        idMatch,
        'domain detail URL should carry a numeric id'
      ).not.toBeNull()
      domainId = Number(idMatch![1])

      // `POST /domains` auto-requests the HTTP-01 challenge server-side, so
      // the challenge panel + its verify button should already be present --
      // no separate "create order" click needed for a freshly created domain.
      const verifyButton = page.getByRole('button', {
        name: 'Verify & provision TLS',
      })
      await expect(verifyButton).toBeVisible({ timeout: 30_000 })
      await verifyButton.click()

      await expect(page.getByText('Active TLS certificate')).toBeVisible({
        timeout: 30_000,
      })

      await page.getByRole('button', { name: 'Show certificate (PEM)' }).click()
      const pem = await page
        .locator('pre')
        .filter({ hasText: 'BEGIN CERTIFICATE' })
        .textContent()
      expect(pem, 'the active certificate PEM should be shown').toBeTruthy()

      // The real proof: parse the PEM and confirm the issuer is Pebble's test
      // root, not a real CA -- "the UI says active" alone would also be true
      // for a cosmetic status flip with no genuine ACME exchange behind it.
      const cert = new X509Certificate(pem!)
      expect(
        cert.issuer,
        'certificate should be issued by Pebble, not a real CA'
      ).toMatch(/pebble/i)
    } finally {
      if (domainId !== null) {
        await page.request.delete(`/api/domains/${encodeURIComponent(domain)}`)
      }
    }
  })
})
