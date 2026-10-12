// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { shouldRetryQuery } from './query-retry'
import {
  getPgUpgrade,
  isPgUpgradeNotFound,
  pgUpgradePollInterval,
} from './pg-upgrades'

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

function mockFetch(status: number, body: unknown) {
  globalThis.fetch = (async () =>
    new Response(JSON.stringify(body), {
      status,
      headers: { 'content-type': 'application/problem+json' },
    })) as unknown as typeof fetch
}

async function failure(status: number, body: unknown): Promise<unknown> {
  mockFetch(status, body)
  try {
    await getPgUpgrade(1, 999)
  } catch (error) {
    return error
  }
  throw new Error('expected getPgUpgrade to reject')
}

test('a 404 for an unknown upgrade is recognised as not found and not retried', async () => {
  const error = await failure(404, {
    title: 'Not Found',
    detail: 'Upgrade 999 not found for service 1',
  })
  expect(error).toBeInstanceOf(Error)
  expect((error as Error).message).toBe('Upgrade 999 not found for service 1')
  expect(isPgUpgradeNotFound(error)).toBe(true)
  // The page would otherwise sit on skeletons for the whole retry backoff.
  expect(shouldRetryQuery(0, error)).toBe(false)
})

test('other failures are not reported as not found and keep their retries', async () => {
  const error = await failure(500, { title: 'Internal Server Error' })
  expect(isPgUpgradeNotFound(error)).toBe(false)
  expect(shouldRetryQuery(0, error)).toBe(true)
  expect(isPgUpgradeNotFound(new TypeError('Failed to fetch'))).toBe(false)
  expect(isPgUpgradeNotFound(undefined)).toBe(false)
})

test('polling only runs for a loaded, non-terminal upgrade', () => {
  expect(pgUpgradePollInterval('running', 2000)).toBe(2000)
  expect(pgUpgradePollInterval('pending', 3000)).toBe(3000)
  for (const status of ['completed', 'failed', 'cancelled']) {
    expect(pgUpgradePollInterval(status, 2000)).toBe(false)
  }
  // No data (loading or errored): polling would flip an errored query back
  // to pending forever and keep the skeleton on screen.
  expect(pgUpgradePollInterval(undefined, 2000)).toBe(false)
})
