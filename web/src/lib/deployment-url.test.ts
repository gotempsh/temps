// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { DeploymentResponse } from '@/api/client'
import { resolvePrimaryUrl, resolveStableUrl } from './deployment-url'

type DeploymentOverrides = Omit<Partial<DeploymentResponse>, 'environment'> & {
  environment?: Partial<DeploymentResponse['environment']>
}

function deployment(overrides: DeploymentOverrides = {}): DeploymentResponse {
  const { environment, ...rest } = overrides
  return {
    created_at: 0,
    environment_id: 2,
    id: 3,
    is_current: false,
    project_id: 2,
    status: 'completed',
    url: 'http://observability-starter-1.127.0.0.1.sslip.io',
    environment: {
      domains: [],
      id: 2,
      name: 'production',
      slug: 'production',
      ...environment,
    },
    ...rest,
  }
}

describe('resolvePrimaryUrl', () => {
  test('current deployment prefers the stable environment domain', () => {
    // Regression: the project header used to link to `deployment.url`, whose
    // slug-derived host (`{project}-{n}`) is ephemeral and, on a sslip.io
    // install, does not even resolve to the right IP.
    expect(
      resolvePrimaryUrl(
        deployment({
          is_current: true,
          environment: {
            domains: [
              'http://observability-starter-production.127.0.0.1.sslip.io',
            ],
          },
        })
      )
    ).toBe('http://observability-starter-production.127.0.0.1.sslip.io')
  })

  test('the first environment domain wins over the slug host', () => {
    expect(
      resolvePrimaryUrl(
        deployment({
          is_current: true,
          environment: { domains: ['https://app.example.com'] },
        })
      )
    ).toBe('https://app.example.com')
  })

  test('domains[0] is used, matching how the backend orders the array', () => {
    // `get_environments_with_domains` does `domains.insert(0, env_url)`, so the
    // generated environment URL is always index 0 and any active custom domains
    // follow it. Pin that real shape: today the generated host wins, and a
    // configured custom domain is NOT surfaced by Visit. Changing that is a
    // backend ordering decision, not a frontend one.
    expect(
      resolvePrimaryUrl(
        deployment({
          is_current: true,
          environment: {
            domains: [
              'http://myapp-production.127-0-0-1.sslip.io',
              'https://app.example.com',
            ],
          },
        })
      )
    ).toBe('http://myapp-production.127-0-0-1.sslip.io')
  })

  test('superseded deployment keeps its own deployment-specific URL', () => {
    // Old builds are not served at the environment domain, so linking there
    // would silently show the user a different version than they asked for.
    expect(
      resolvePrimaryUrl(
        deployment({
          is_current: false,
          environment: {
            domains: [
              'http://observability-starter-production.127.0.0.1.sslip.io',
            ],
          },
        })
      )
    ).toBe('http://observability-starter-1.127.0.0.1.sslip.io')
  })

  test('current deployment without env domains falls back to its own URL', () => {
    expect(resolvePrimaryUrl(deployment({ is_current: true }))).toBe(
      'http://observability-starter-1.127.0.0.1.sslip.io'
    )
  })

  test('bare hostnames are normalized to absolute URLs', () => {
    expect(
      resolvePrimaryUrl(
        deployment({
          is_current: true,
          environment: { domains: ['app.example.com'] },
        })
      )
    ).toBe('https://app.example.com')
  })

  test('returns null when there is nothing to link to', () => {
    expect(resolvePrimaryUrl(deployment({ url: '' }))).toBeNull()
  })

  test('empty deployment URL still falls back to the environment domain', () => {
    expect(
      resolvePrimaryUrl(
        deployment({ url: '', environment: { domains: ['app.example.com'] } })
      )
    ).toBe('https://app.example.com')
  })

  // The result is rendered as a followable link and environment domains are
  // partly user-supplied (custom domains), so a non-http(s) scheme must never
  // reach the href. A `startsWith('http')` check passed several of these.
  const hostileDomains: string[] = [
    'javascript:alert(1)',
    'httpfoo://evil.com',
    '//evil.com',
    'data:text/html,<script>alert(1)</script>',
    'file:///etc/passwd',
    'vbscript:msgbox(1)',
    // The URL parser treats `\` as `/` for special schemes and ignores any
    // number of leading ones, so these resolve to origin evil.com once a
    // scheme is prefixed.
    '\\\\evil.com',
    '/\\evil.com',
    // Parses to origin evil.com without a base, but resolves base-relative in
    // a browser — the validated origin and the linked origin disagree.
    'https:evil.com',
    // The URL parser strips tab/LF/CR from anywhere in the input, so a leading
    // control character would otherwise carry a protocol-relative value past a
    // raw prefix check and still parse as `//evil.com`.
    `${String.fromCharCode(9)}//evil.com`,
    `${String.fromCharCode(10)}//evil.com`,
    `${String.fromCharCode(13)}//evil.com`,
    `${String.fromCharCode(0)}//evil.com`,
    `${String.fromCharCode(9)}/\\evil.com`,
    ` //evil.com`,
  ]
  for (const hostile of hostileDomains) {
    test(`rejects non-http(s) candidate ${hostile}`, () => {
      expect(
        resolvePrimaryUrl(
          deployment({
            is_current: true,
            url: '',
            environment: { domains: [hostile] },
          })
        )
      ).toBeNull()
    })
  }

  test('a hostile environment domain falls through to the deployment URL', () => {
    // One bad stored domain must not suppress an otherwise valid link.
    expect(
      resolvePrimaryUrl(
        deployment({
          is_current: true,
          url: 'https://good.example.com',
          environment: { domains: ['javascript:alert(1)'] },
        })
      )
    ).toBe('https://good.example.com')
  })

  test('a bare host:port keeps its port', () => {
    // Regression: a scheme regex without the `//` reads `app.example.com:` as
    // the scheme and drops the value entirely.
    expect(
      resolvePrimaryUrl(
        deployment({
          is_current: true,
          environment: { domains: ['app.example.com:8080'] },
        })
      )
    ).toBe('https://app.example.com:8080')
  })

  test('http scheme is preserved, not upgraded', () => {
    // Local / self-hosted installs are frequently HTTP-only; rewriting to
    // https would produce a dead link.
    expect(
      resolvePrimaryUrl(
        deployment({
          is_current: true,
          environment: { domains: ['http://app.127-0-0-1.sslip.io'] },
        })
      )
    ).toBe('http://app.127-0-0-1.sslip.io')
  })
})

describe('resolveStableUrl', () => {
  test('uses the environment domain even when the build is not current', () => {
    // `get_last_deployment` orders by created_at with no status filter, so the
    // newest deployment is not necessarily the current one: while a deploy runs,
    // or after one fails, `is_current` is false. The project header must still
    // link to the live site, not to that build's ephemeral, never-routed host.
    expect(
      resolveStableUrl(
        deployment({
          is_current: false,
          status: 'failed',
          url: 'http://observability-starter-7.127-0-0-1.sslip.io',
          environment: {
            domains: [
              'http://observability-starter-production.127-0-0-1.sslip.io',
            ],
          },
        })
      )
    ).toBe('http://observability-starter-production.127-0-0-1.sslip.io')
  })

  test('never falls back to the ephemeral deployment host', () => {
    expect(
      resolveStableUrl(
        deployment({
          is_current: false,
          url: 'http://observability-starter-7.127-0-0-1.sslip.io',
          environment: { domains: [] },
        })
      )
    ).toBeNull()
  })

  test('rejects a hostile environment domain rather than linking it', () => {
    expect(
      resolveStableUrl(
        deployment({
          is_current: true,
          environment: { domains: ['javascript:alert(1)'] },
        })
      )
    ).toBeNull()
  })
})
