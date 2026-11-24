// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Which console specs run, selected with `E2E_SUITE`.
 *
 * `smoke` is the pull-request gate. It keeps the checks that guard real
 * behaviour and survive UI redesigns: the console boots (the #504 guard),
 * login works, every sidebar destination renders, and the deploy and
 * infrastructure flows -- which drive the API through the browser session and
 * assert on what the backend did, not on button labels. Nothing in it
 * asserts exact UI copy beyond the login form.
 *
 * `full` (the default) is every spec. Most of them are route-mocked
 * behaviour tests of individual screens, written against literal UI text, so
 * they break whenever that screen is redesigned. CI still runs them on every
 * change to the console, but as a non-blocking report: a reworded heading
 * should be fixed in the spec, not hold up a merge.
 *
 * A spec named here that no longer exists fails the run (see
 * `missingSmokeSpecs`), so renaming one cannot silently drop it from the gate.
 */

import { existsSync } from 'node:fs'
import { join } from 'node:path'

export type E2eSuite = 'smoke' | 'full'

export const SMOKE_SPECS = {
  /** Under e2e/anonymous/ */
  anonymous: ['console-boot.spec.ts', 'auth.spec.ts'],
  /** Under e2e/authenticated/ */
  authenticated: [
    'navigation.spec.ts',
    // Real deployments through the API: build images, push them to the
    // fixture registry, deploy, and assert health-gated rollout and alarms.
    'health-probes.spec.ts',
    'runtime-health.spec.ts',
    // Real sandbox start and readiness exec behind the preview gateway.
    'preview-share-link.spec.ts',
    // Compose deployment security policy, asserted through the API.
    'compose-security.spec.ts',
    // Real ACME issuance against Pebble, real SMTP delivery into Mailpit.
    'domain-tls-pebble.spec.ts',
    'email-provider-mailpit.spec.ts',
  ],
} as const

export function selectedSuite(value: string | undefined): E2eSuite {
  if (value === undefined || value === '' || value === 'full') return 'full'
  if (value === 'smoke') return 'smoke'
  throw new Error(
    `E2E_SUITE must be "smoke" or "full" (got ${JSON.stringify(value)})`
  )
}

/** Matches exactly the named files, wherever the project's testDir is. */
export function specMatcher(files: readonly string[]): RegExp {
  const escaped = files.map((file) =>
    file.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  )
  return new RegExp(`(^|[\\\\/])(${escaped.join('|')})$`)
}

/** Smoke specs that do not exist under `e2eDir`, as paths relative to it. */
export function missingSmokeSpecs(e2eDir: string): string[] {
  return [
    ...SMOKE_SPECS.anonymous.map((file) => join('anonymous', file)),
    ...SMOKE_SPECS.authenticated.map((file) => join('authenticated', file)),
  ].filter((relative) => !existsSync(join(e2eDir, relative)))
}
