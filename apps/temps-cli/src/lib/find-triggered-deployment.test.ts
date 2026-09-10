// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  commitMatches,
  selectTriggeredDeployment,
  waitForTriggeredDeployment,
  type DeploymentCandidate,
} from './find-triggered-deployment.js'

const NEW_COMMIT = 'abcdef0123456789abcdef0123456789abcdef01'
const OLD_COMMIT = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
const OTHER_COMMIT = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'

const deployments: DeploymentCandidate[] = [
  { id: 352, commit_hash: NEW_COMMIT },
  { id: 341, commit_hash: OLD_COMMIT },
  { id: 300, commit_hash: OTHER_COMMIT },
]

describe('commitMatches', () => {
  test('accepts empty requested commit', () => {
    expect(commitMatches('abc', null)).toBe(true)
    expect(commitMatches('abc', '')).toBe(true)
    expect(commitMatches('abc', '   ')).toBe(true)
  })

  test('matches full and abbreviated SHAs either direction', () => {
    expect(commitMatches(NEW_COMMIT, 'abcdef0')).toBe(true)
    expect(commitMatches('abcdef0', NEW_COMMIT)).toBe(true)
    expect(commitMatches('ABCDEF0123', 'abcdef0')).toBe(true)
  })

  test('rejects mismatched hashes and missing deployment hash', () => {
    expect(commitMatches('deadbeef', 'abcdef0')).toBe(false)
    expect(commitMatches(null, 'abcdef0')).toBe(false)
  })
})

describe('selectTriggeredDeployment', () => {
  test('rejects the pre-trigger latest deployment (regression: latching onto prior id)', () => {
    expect(
      selectTriggeredDeployment(deployments, {
        afterId: 341,
        commit: NEW_COMMIT,
      })?.id,
    ).toBe(352)
  })

  test('returns null when only older deployments exist', () => {
    expect(
      selectTriggeredDeployment(
        [
          { id: 341, commit_hash: OLD_COMMIT },
          { id: 300, commit_hash: OTHER_COMMIT },
        ],
        { afterId: 341 },
      ),
    ).toBeNull()
  })

  test('without a baseline (first deploy) accepts the newest row', () => {
    expect(selectTriggeredDeployment(deployments, { afterId: 0 })?.id).toBe(352)
  })

  test('skips a newer row that does not match the requested commit', () => {
    const mixed: DeploymentCandidate[] = [
      { id: 360, commit_hash: 'cccccccccccccccccccccccccccccccccccccccc' },
      { id: 352, commit_hash: NEW_COMMIT },
      { id: 341, commit_hash: OLD_COMMIT },
    ]
    expect(
      selectTriggeredDeployment(mixed, {
        afterId: 341,
        commit: 'abcdef0',
      })?.id,
    ).toBe(352)
  })

  test('without commit filter returns the first id after the baseline', () => {
    expect(selectTriggeredDeployment(deployments, { afterId: 300 })?.id).toBe(352)
  })
})

describe('waitForTriggeredDeployment', () => {
  test('keeps polling until a post-baseline deployment appears', async () => {
    let calls = 0
    const result = await waitForTriggeredDeployment(
      async () => {
        calls += 1
        if (calls < 3) {
          return {
            deployments: [{ id: 341, commit_hash: OLD_COMMIT }],
          }
        }
        return {
          deployments: [
            { id: 352, commit_hash: NEW_COMMIT },
            { id: 341, commit_hash: OLD_COMMIT },
          ],
        }
      },
      {
        afterId: 341,
        commit: 'abcdef0',
        maxAttempts: 5,
        intervalMs: 1,
        sleep: async () => {},
      },
    )

    expect(result).toEqual({ ok: true, deploymentId: 352 })
    expect(calls).toBe(3)
  })

  test('returns fetch_failed when the list call errors', async () => {
    const result = await waitForTriggeredDeployment(
      async () => ({ error: new Error('boom') }),
      { afterId: 0, maxAttempts: 1, sleep: async () => {} },
    )
    expect(result.ok).toBe(false)
    if (!result.ok) {
      expect(result.error).toBe('fetch_failed')
    }
  })

  test('returns not_found after exhausting attempts', async () => {
    const result = await waitForTriggeredDeployment(
      async () => ({
        deployments: [{ id: 341, commit_hash: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' }],
      }),
      { afterId: 341, maxAttempts: 2, intervalMs: 1, sleep: async () => {} },
    )
    expect(result).toEqual({ ok: false, error: 'not_found' })
  })
})
