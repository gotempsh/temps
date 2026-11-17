// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type {
  EmailDomainResponse,
  EmailProviderResponse,
  EmailStatsResponse,
} from '@/api/client'
import { emailOverviewSections, emailSetupSteps } from './email-setup'

const provider = {
  id: 1,
  name: 'Primary relay',
  provider_type: 'ses',
} as EmailProviderResponse

const domain = (status: string, provider_id = 1) =>
  ({
    id: 7,
    domain: 'mail.example.com',
    status,
    provider_id,
  }) as EmailDomainResponse

const stats = (partial: Partial<EmailStatsResponse> = {}) =>
  ({
    total: 0,
    sent: 0,
    failed: 0,
    captured: 0,
    queued: 0,
    sending: 0,
    delivery_unknown: 0,
    ...partial,
  }) as EmailStatsResponse

const step = (
  id: string,
  input: Partial<Parameters<typeof emailSetupSteps>[0]>
) =>
  emailSetupSteps({
    providers: [],
    domains: [],
    stats: stats(),
    ...input,
  }).find((candidate) => candidate.id === id)!

test('nothing configured starts at the provider and says what happens meanwhile', () => {
  const provider = step('provider', {})
  expect(provider.done).toBe(false)
  expect(provider.action?.to).toBe('/email/providers/new')
  expect(provider.detail).toContain('captured')
})

test('a connected provider is done and names itself', () => {
  expect(step('provider', { providers: [provider] }).detail).toBe(
    'Connected Primary relay'
  )
  expect(
    step('provider', { providers: [provider, { ...provider, id: 2 }] }).detail
  ).toBe('Connected Primary relay and 1 more')
})

test('no domain asks for one', () => {
  expect(step('domain', { providers: [provider] }).action?.to).toBe(
    '/email/domains/new'
  )
})

test('a waiting domain links to its records, not to a new domain', () => {
  const waiting = step('domain', {
    providers: [provider],
    domains: [domain('pending')],
  })
  expect(waiting.done).toBe(false)
  expect(waiting.action).toEqual({
    label: 'Open DNS records',
    to: '/email/domains/7',
  })
  expect(waiting.detail).toContain('mail.example.com')
})

test('a failed domain says it failed', () => {
  expect(
    step('domain', { providers: [provider], domains: [domain('failed')] })
      .detail
  ).toContain('failed verification')
})

test('an SMTP domain is enabled rather than verified', () => {
  const waiting = step('domain', {
    providers: [{ ...provider, provider_type: 'smtp' }],
    domains: [domain('pending')],
  })
  expect(waiting.action?.label).toBe('Enable sending')
  expect(waiting.detail).toContain('paused')
})

test('one verified domain among several completes the step', () => {
  const done = step('domain', {
    providers: [provider],
    domains: [domain('pending'), { ...domain('verified'), id: 8 }],
  })
  expect(done.done).toBe(true)
  expect(done.action).toBeUndefined()
})

test('only delivered email completes sending, not captured or failed', () => {
  expect(step('send', { stats: stats({ total: 3, captured: 3 }) }).done).toBe(
    false
  )
  const failed = step('send', { stats: stats({ total: 2, failed: 2 }) })
  expect(failed.done).toBe(false)
  expect(failed.detail).toContain('2 failed')
  expect(step('send', { stats: stats({ total: 1, sent: 1 }) }).done).toBe(true)
})

test('with nothing set up, only the checklist shows', () => {
  expect(
    emailOverviewSections({ providers: [], domains: [], stats: stats() })
  ).toEqual({ domains: false, providers: false, sent: false, events: false })
})

test('during setup, a section shows once it has something to manage', () => {
  expect(
    emailOverviewSections({
      providers: [provider],
      domains: [],
      stats: stats({ total: 2, captured: 2 }),
    })
  ).toEqual({ domains: false, providers: true, sent: true, events: true })
  expect(
    emailOverviewSections({
      providers: [provider],
      domains: [domain('pending')],
      stats: stats(),
    })
  ).toEqual({ domains: true, providers: true, sent: false, events: false })
})

test('once setup is done every section shows, even an empty one', () => {
  expect(
    emailOverviewSections({
      providers: [provider],
      domains: [domain('verified')],
      stats: stats({ total: 1, sent: 1 }),
    })
  ).toEqual({ domains: true, providers: true, sent: true, events: true })
})
