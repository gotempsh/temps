// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { describe, expect, test } from 'bun:test'

import {
  deploymentTokenAuditActor,
  describeVisitorEnrichment,
} from './visitor-enrich-audit'

describe('deploymentTokenAuditActor', () => {
  test('names the token that wrote to the visitor', () => {
    expect(
      deploymentTokenAuditActor('VISITOR_ENRICHED', {
        actor_kind: 'deployment_token',
        deployment_token_id: 42,
        deployment_token_name: 'checkout-app',
      })
    ).toEqual({ id: 42, name: 'checkout-app' })
  })

  test('still identifies an unnamed token rather than falling back to system', () => {
    expect(
      deploymentTokenAuditActor('VISITOR_ENRICHED', {
        actor_kind: 'deployment_token',
        deployment_token_id: 7,
      })
    ).toEqual({ id: 7, name: null })
  })

  test('reads a payload that arrived as JSON text', () => {
    expect(
      deploymentTokenAuditActor(
        'VISITOR_ENRICHED',
        '{"actor_kind":"deployment_token","deployment_token_id":1,"deployment_token_name":"app"}'
      )
    ).toEqual({ id: 1, name: 'app' })
  })

  test('leaves user-driven enrichments to the normal actor presentation', () => {
    expect(
      deploymentTokenAuditActor('VISITOR_ENRICHED', {
        actor_kind: 'user',
        deployment_token_id: null,
      })
    ).toBeNull()
  })

  test('never crashes on missing or malformed payloads', () => {
    // A bare array is kept out of `test.each` on purpose: bun spreads array
    // cases into arguments, so it would arrive as "no arguments".
    for (const data of [
      undefined,
      null,
      'not json',
      '[]',
      [],
      7,
      {},
      { actor_kind: 42 },
    ]) {
      expect(deploymentTokenAuditActor('VISITOR_ENRICHED', data)).toBeNull()
    }
  })

  test('does not reinterpret other operations as token activity', () => {
    expect(
      deploymentTokenAuditActor('PROJECT_CREATED', {
        actor_kind: 'deployment_token',
        deployment_token_id: 1,
        deployment_token_name: 'app',
      })
    ).toBeNull()
  })
})

describe('describeVisitorEnrichment', () => {
  test('says what was removed, separately from what was set', () => {
    expect(
      describeVisitorEnrichment({
        visitor_row_id: 11,
        custom_data_keys: ['email'],
        custom_data_key_count: 1,
        removed_keys: ['plan', 'segment'],
        removed_key_count: 2,
      })
    ).toBe('Enriched visitor 11 (email; removed: plan, segment)')
    expect(
      describeVisitorEnrichment({
        visitor_row_id: 11,
        custom_data_keys: [],
        custom_data_key_count: 0,
        removed_keys: ['plan'],
        removed_key_count: 1,
      })
    ).toBe('Removed data from visitor 11 (plan)')
  })

  test('names the visitor and the keys, collapsing the tail', () => {
    expect(
      describeVisitorEnrichment({
        visitor_row_id: 11,
        custom_data_keys: ['email', 'name', 'plan', 'company', 'role'],
        custom_data_key_count: 5,
      })
    ).toBe('Enriched visitor 11 (email, name, plan, +2 more)')
  })

  test('lists every key when they all fit', () => {
    expect(
      describeVisitorEnrichment({
        visitor_row_id: 3,
        custom_data_keys: ['email', 'name'],
        custom_data_key_count: 2,
      })
    ).toBe('Enriched visitor 3 (email, name)')
  })

  test('trusts the total count when the key list was truncated', () => {
    expect(
      describeVisitorEnrichment({
        visitor_row_id: 3,
        custom_data_keys: ['a', 'b'],
        custom_data_key_count: 30,
      })
    ).toBe('Enriched visitor 3 (a, b, +28 more)')
  })

  test('falls back to the count alone when no key names were recorded', () => {
    expect(
      describeVisitorEnrichment({
        visitor_row_id: 3,
        custom_data_keys: [],
        custom_data_key_count: 4,
      })
    ).toBe('Enriched visitor 3 (4 keys)')
  })

  test('never leaks a value, only key names', () => {
    const described = describeVisitorEnrichment({
      visitor_row_id: 1,
      custom_data_keys: ['email'],
      custom_data_key_count: 1,
      email: 'ada@example.com',
    })
    expect(described).toBe('Enriched visitor 1 (email)')
    expect(described).not.toContain('ada@example.com')
  })

  test('degrades to a generic line for malformed payloads', () => {
    for (const data of [
      undefined,
      null,
      'not json',
      [],
      {},
      { visitor_row_id: 'x' },
    ]) {
      expect(describeVisitorEnrichment(data)).toBe('Enriched a visitor')
    }
  })
})
