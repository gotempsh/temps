// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  BASELINE_PATH,
  describeSecurityDrift,
  operationsWithoutSecurity,
  securityDrift,
} from './openapi-security'
import { SPEC_PATH } from './openapi-canonical'

const bearer = [{ bearer_auth: [] }]

const spec = {
  paths: {
    '/projects/{project_id}/env-vars': {
      get: { security: bearer },
      post: { security: [] },
      parameters: [],
    },
    '/status/{slug}': { get: {} },
    '/auth/login': { post: { security: bearer } },
  },
}

describe('operationsWithoutSecurity', () => {
  test('lists operations with a missing or empty security list, sorted', () => {
    expect(operationsWithoutSecurity(spec)).toEqual([
      'GET /status/{slug}',
      'POST /projects/{project_id}/env-vars',
    ])
  })

  test('is empty for a document without paths', () => {
    expect(operationsWithoutSecurity({})).toEqual([])
    expect(operationsWithoutSecurity(null)).toEqual([])
  })
})

describe('securityDrift', () => {
  test('flags an unsecured operation the baseline does not list', () => {
    const drift = securityDrift(spec, ['GET /status/{slug}'])
    expect(drift).toEqual({ undeclared: ['POST /projects/{project_id}/env-vars'], stale: [] })
    const message = describeSecurityDrift(drift) ?? ''
    expect(message).toContain('POST /projects/{project_id}/env-vars')
    expect(message).toContain('security(("bearer_auth" = []))')
  })

  test('flags a baseline entry that now declares security or is gone', () => {
    const drift = securityDrift(spec, [
      'GET /status/{slug}',
      'POST /projects/{project_id}/env-vars',
      'POST /auth/login',
      'DELETE /removed',
    ])
    expect(drift).toEqual({ undeclared: [], stale: ['DELETE /removed', 'POST /auth/login'] })
    expect(describeSecurityDrift(drift)).toContain('remove them')
  })

  test('passes when the baseline matches', () => {
    const drift = securityDrift(spec, ['GET /status/{slug}', 'POST /projects/{project_id}/env-vars'])
    expect(describeSecurityDrift(drift)).toBeNull()
  })
})

describe('the committed spec', () => {
  test('declares authentication on every operation not listed as unsecured', async () => {
    const committed = await Bun.file(SPEC_PATH).json()
    const baseline = (await Bun.file(BASELINE_PATH).json()) as string[]
    expect(describeSecurityDrift(securityDrift(committed, baseline))).toBeNull()
  })

  test('the environment, variable and secret operations declare bearer auth', async () => {
    const committed = await Bun.file(SPEC_PATH).json()
    const unsecured = operationsWithoutSecurity(committed).filter((operation) =>
      /^\w+ \/projects\/\{project_id\}\/(environments(\/\{env_id\}(\/(domains.*|settings|subdomain|wake|sleep))?)?|env-vars.*|secrets.*)$/.test(
        operation,
      ),
    )
    expect(unsecured).toEqual([])
  })

  test('the baseline is sorted and has no duplicates', async () => {
    const baseline = (await Bun.file(BASELINE_PATH).json()) as string[]
    expect(baseline).toEqual([...new Set(baseline)].sort())
  })
})
