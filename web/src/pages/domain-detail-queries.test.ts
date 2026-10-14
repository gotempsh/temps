// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  getDomainByIdQueryKey,
  getDomainOrderQueryKey,
  getHttpChallengeDebugQueryKey,
  getPublicIpQueryKey,
  getProjectsQueryKey,
  listDnsProvidersQueryKey,
  listRenewalAttemptsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { isDomainDetailQuery } from './domain-detail-queries'

test('matches the renewal attempts query for any page', () => {
  expect(
    isDomainDetailQuery(
      listRenewalAttemptsQueryKey({
        path: { domain: 'app.example.test' },
        query: { page: 2, page_size: 10 },
      })
    )
  ).toBe(true)
})

test('matches every query the domain detail page loads', () => {
  const keys = [
    getDomainByIdQueryKey({ path: { domain: 1 } }),
    getDomainOrderQueryKey({ path: { domain_id: 1 } }),
    getHttpChallengeDebugQueryKey({
      path: { domain: 'app.example.test' },
      query: { challenge_type: 'http-01' },
    } as never),
    getPublicIpQueryKey(),
    listDnsProvidersQueryKey(),
  ]
  for (const key of keys) expect(isDomainDetailQuery(key)).toBe(true)
})

test('ignores unrelated and malformed keys', () => {
  expect(isDomainDetailQuery(getProjectsQueryKey())).toBe(false)
  expect(isDomainDetailQuery(['getDomainById'])).toBe(false)
  expect(isDomainDetailQuery([])).toBe(false)
  expect(isDomainDetailQuery([null])).toBe(false)
  expect(isDomainDetailQuery(undefined)).toBe(false)
})
