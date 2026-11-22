// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  resolveCreateEvents,
  selectEventsByIndex,
  parseWebhookEventsList,
  groupEventsByCategory,
  formatDeliveryPayload,
} from './index.js'
import { registerWebhooksCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const allEventTypes = ['deployment.started', 'deployment.completed', 'domain.verified']

describe('resolveCreateEvents', () => {
  test('resolves "all" (case-insensitive) to every known event type', () => {
    expect(resolveCreateEvents('ALL', allEventTypes)).toEqual({ events: allEventTypes })
  })

  test('splits and trims a comma-separated list of valid events', () => {
    expect(resolveCreateEvents('deployment.started, domain.verified', allEventTypes)).toEqual({
      events: ['deployment.started', 'domain.verified'],
    })
  })

  test('rejects an unknown event type instead of sending a bad subscription', () => {
    expect(resolveCreateEvents('deployment.started,bogus.event', allEventTypes)).toEqual({
      invalidEvent: 'bogus.event',
    })
  })
})

describe('selectEventsByIndex', () => {
  const eventTypes = [
    { event_type: 'deployment.started' },
    { event_type: 'deployment.completed' },
    { event_type: 'domain.verified' },
  ]

  test('converts 1-based comma-separated picks to event type strings', () => {
    expect(selectEventsByIndex('1,3', eventTypes)).toEqual(['deployment.started', 'domain.verified'])
  })

  test('drops out-of-range or non-numeric picks rather than throwing', () => {
    // Interactive input is free text; a typo like "0" or "99" must not crash
    // the prompt, it should just contribute nothing to the selection.
    expect(selectEventsByIndex('0,2,99,abc', eventTypes)).toEqual(['deployment.completed'])
  })
})

describe('parseWebhookEventsList', () => {
  test('resolves "all" to the full type list', () => {
    expect(parseWebhookEventsList('all', allEventTypes)).toEqual(allEventTypes)
  })

  test('splits and trims an explicit list without validating against known types', () => {
    // Unlike resolveCreateEvents (used by `create`), the `update` path does
    // not check membership — this documents that existing asymmetry.
    expect(parseWebhookEventsList('deployment.started, totally.unknown', allEventTypes)).toEqual([
      'deployment.started',
      'totally.unknown',
    ])
  })
})

describe('groupEventsByCategory', () => {
  test('buckets events under their category', () => {
    const events = [
      { event_type: 'a', category: 'Deployments' },
      { event_type: 'b', category: 'Deployments' },
      { event_type: 'c', category: 'Domains' },
    ]
    const grouped = groupEventsByCategory(events)
    expect([...grouped.keys()]).toEqual(['Deployments', 'Domains'])
    expect(grouped.get('Deployments')).toEqual([
      { event_type: 'a', category: 'Deployments' },
      { event_type: 'b', category: 'Deployments' },
    ])
    expect(grouped.get('Domains')).toEqual([{ event_type: 'c', category: 'Domains' }])
  })

  test('falls back to "Other" when category is missing', () => {
    const events = [{ event_type: 'a', category: undefined }]
    const grouped = groupEventsByCategory(events)
    expect(grouped.has('Other')).toBe(true)
  })
})

describe('formatDeliveryPayload', () => {
  test('pretty-prints valid JSON payloads', () => {
    expect(formatDeliveryPayload('{"a":1}')).toBe(JSON.stringify({ a: 1 }, null, 2))
  })

  test('falls back to the raw string when payload is not valid JSON', () => {
    expect(formatDeliveryPayload('not json')).toBe('not json')
  })
})

// --- Credential file/stdin sources (shared value-source helper) ---

const CREDENTIAL_SENTINEL = 's3cr3t-test-value'

async function credentialFile(contents: string): Promise<string> {
  const { mkdtemp, writeFile, chmod } = await import('node:fs/promises')
  const { tmpdir } = await import('node:os')
  const { join } = await import('node:path')
  const path = join(await mkdtemp(join(tmpdir(), 'temps-credential-')), 'credential.txt')
  await writeFile(path, contents)
  await chmod(path, 0o600)
  return path
}

function credentialBody(run: { bodies: Array<{ request: string; body: unknown }> }, request: string): Record<string, unknown> {
  const sent = run.bodies.find((b) => b.request === request)
  expect(sent).toBeDefined()
  return sent!.body as Record<string, unknown>
}

describe('webhooks create credential sources', () => {
  test('--secret-file sends the signing secret from the file exactly', async () => {
    const path = await credentialFile(CREDENTIAL_SENTINEL)
    const run = await runWithApi(registerWebhooksCommands,
      ['webhooks', 'create', '--project-id', '4', '-u', 'https://hooks.example.com/x', '-e', 'all', '--secret-file', path, '-y'],
      { 'GET /webhook-event-types': [{ event_type: 'deployment.succeeded', category: 'deployments', description: 'x' }], 'POST /projects/4/webhooks': (_url: URL, body: unknown) => ({ id: 1, project_id: 4, url: 'https://hooks.example.com/x', events: [], enabled: true, has_secret: true, created_at: '2026-01-01T00:00:00Z', updated_at: '2026-01-01T00:00:00Z', ...(body as object), secret: undefined }) })
    expect(run.error).toBeUndefined()
    expect(credentialBody(run, 'POST /projects/4/webhooks').secret).toBe(CREDENTIAL_SENTINEL)
  })
})

describe('webhooks create with an empty secret file', () => {
  test('is refused before the webhook is created, instead of creating an unsigned one', async () => {
    const path = await credentialFile('\n')
    const run = await runWithApi(registerWebhooksCommands,
      ['webhooks', 'create', '--project-id', '4', '-u', 'https://hooks.example.com/x', '-e', 'all', '--secret-file', path, '--trim-trailing-newline', '-y'],
      {})
    expect(run.error?.message).toBe('the webhook signing secret read from the file is empty after trimming the trailing newline')
    expect(run.requests).toEqual([])
  })
})
