// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  buildEnrichPayload,
  enrichJsonOutput,
  visitorNotFoundMessage,
} from './enrich.js'
import type { EnrichVisitorResponse } from '../../api/types.gen.js'

describe('buildEnrichPayload (sources and precedence)', () => {
  test('--data alone becomes the custom_data object verbatim', () => {
    const result = buildEnrichPayload({
      data: '{"user_id":"user_123","email":"ada@example.com"}',
    })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.payload).toEqual({
      user_id: 'user_123',
      email: 'ada@example.com',
    })
    expect(result.setKeys).toEqual(['user_id', 'email'])
    expect(result.unsetKeys).toEqual([])
  })

  test('--file alone becomes the custom_data object verbatim', () => {
    const result = buildEnrichPayload({
      fileContents: '{\n  "plan": "pro"\n}\n',
      filePath: './visitor.json',
    })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.payload).toEqual({ plan: 'pro' })
  })

  test('--data overrides --file for the same key, and both keys survive', () => {
    const result = buildEnrichPayload({
      fileContents: '{"plan":"free","segment":"smb"}',
      data: '{"plan":"pro"}',
    })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.payload).toEqual({ plan: 'pro', segment: 'smb' })
  })

  test('--set overrides --data and --file for the same key', () => {
    const result = buildEnrichPayload({
      fileContents: '{"plan":"free"}',
      data: '{"plan":"pro"}',
      set: ['plan=enterprise'],
    })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.payload).toEqual({ plan: 'enterprise' })
  })

  test('--set values stay strings: no type coercion of numbers or booleans', () => {
    // The API stores whatever JSON it is given; a shell has no types, so
    // guessing here would silently write 1 where the user meant "1".
    const result = buildEnrichPayload({ set: ['seats=12', 'trial=true'] })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.payload).toEqual({ seats: '12', trial: 'true' })
  })

  test('a later --set wins over an earlier one for the same key', () => {
    const result = buildEnrichPayload({ set: ['plan=free', 'plan=pro'] })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.payload).toEqual({ plan: 'pro' })
  })

  test('--set keeps an empty value as an empty string rather than dropping it', () => {
    const result = buildEnrichPayload({ set: ['note='] })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.payload).toEqual({ note: '' })
  })

  test('--set only splits on the first "=", so values may contain "="', () => {
    const result = buildEnrichPayload({ set: ['token_hint=a=b=c'] })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.payload).toEqual({ token_hint: 'a=b=c' })
  })

  test('--unset sends null for the key, which is how the API removes it', () => {
    const result = buildEnrichPayload({ unset: ['trial_ends_at'] })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.payload).toEqual({ trial_ends_at: null })
    expect(result.setKeys).toEqual([])
    expect(result.unsetKeys).toEqual(['trial_ends_at'])
  })

  test('--unset wins over the same key coming from --data or --file', () => {
    // Removal is the destructive half of the merge: it must not be silently
    // cancelled by a key that happens to also sit in the JSON payload.
    const result = buildEnrichPayload({
      fileContents: '{"trial_ends_at":"2026-01-01"}',
      data: '{"trial_ends_at":"2026-02-01","plan":"pro"}',
      unset: ['trial_ends_at'],
    })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.payload).toEqual({ trial_ends_at: null, plan: 'pro' })
    expect(result.setKeys).toEqual(['plan'])
    expect(result.unsetKeys).toEqual(['trial_ends_at'])
  })

  test('a literal null in --data is reported as a removal, not as a write', () => {
    const result = buildEnrichPayload({ data: '{"plan":"pro","segment":null}' })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.payload).toEqual({ plan: 'pro', segment: null })
    expect(result.setKeys).toEqual(['plan'])
    expect(result.unsetKeys).toEqual(['segment'])
  })

  test('nested values from --data are preserved as-is', () => {
    const result = buildEnrichPayload({
      data: '{"tags":["vip","beta"],"billing":{"seats":12}}',
    })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.payload).toEqual({
      tags: ['vip', 'beta'],
      billing: { seats: 12 },
    })
  })
})

describe('buildEnrichPayload (rejections)', () => {
  test('rejects a call with no --data, --file, --set or --unset', () => {
    const result = buildEnrichPayload({})
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('Nothing to enrich')
    expect(result.error).toContain('--data')
    expect(result.error).toContain('--unset')
  })

  test('rejects empty repeatable flags with nothing else supplied', () => {
    // commander hands these in as [] when the flags were never typed.
    const result = buildEnrichPayload({ set: [], unset: [] })
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('Nothing to enrich')
  })

  test('rejects a JSON array, naming the flag and what arrived', () => {
    const result = buildEnrichPayload({ data: '["user_123"]' })
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('--data must be a JSON object')
    expect(result.error).toContain('an array')
  })

  test('rejects a bare JSON primitive', () => {
    for (const [raw, kind] of [
      ['"user_123"', 'a string'],
      ['42', 'a number'],
      ['true', 'a boolean'],
      ['null', 'null'],
    ] as const) {
      const result = buildEnrichPayload({ data: raw })
      expect(result.ok).toBe(false)
      if (result.ok) return
      expect(result.error).toContain('must be a JSON object')
      expect(result.error).toContain(kind)
    }
  })

  test('rejects a non-object read from --file, naming the path', () => {
    const result = buildEnrichPayload({
      fileContents: '[1,2,3]',
      filePath: './visitor.json',
    })
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('--file "./visitor.json"')
    expect(result.error).toContain('must be a JSON object')
  })

  test('rejects malformed JSON instead of sending it', () => {
    const result = buildEnrichPayload({ data: '{user_id: user_123}' })
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('--data is not valid JSON')
    expect(result.error).toContain('{"user_id":"user_123"}')
  })

  test('rejects an empty --data / --file rather than sending {}', () => {
    expect(buildEnrichPayload({ data: '   ' }).ok).toBe(false)
    expect(buildEnrichPayload({ fileContents: '\n' }).ok).toBe(false)
  })

  test('rejects an empty JSON object: there would be nothing to merge', () => {
    const result = buildEnrichPayload({ data: '{}' })
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('no keys')
  })

  test('rejects a --set pair with no "="', () => {
    const result = buildEnrichPayload({ set: ['justakey'] })
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('Expected format: key=value')
  })

  test('rejects a --set pair with an empty key without echoing the value', () => {
    const result = buildEnrichPayload({ set: ['=ada@example.com'] })
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('the key before "=" cannot be empty')
    expect(result.error).not.toContain('ada@example.com')
  })

  test('rejects --unset given a key=value pair without echoing the value', () => {
    const result = buildEnrichPayload({ unset: ['email=ada@example.com'] })
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('--unset takes a key name only')
    expect(result.error).not.toContain('ada@example.com')
  })

  test('rejects an empty --unset key', () => {
    const result = buildEnrichPayload({ unset: ['  '] })
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('key name cannot be empty')
  })

  test('rejects the same key in both --set and --unset', () => {
    const result = buildEnrichPayload({ set: ['plan=pro'], unset: ['plan'] })
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('both --set and --unset')
  })
})

describe('enrichJsonOutput', () => {
  const response: EnrichVisitorResponse = {
    success: true,
    visitor_id: '550e8400-e29b-41d4-a716-446655440000',
    message: 'Visitor enriched successfully',
  }

  test('reports the server outcome plus the key names this call touched', () => {
    expect(
      enrichJsonOutput('enc_AbC123', response, {
        setKeys: ['user_id'],
        unsetKeys: ['trial_ends_at'],
      }),
    ).toEqual({
      requested_visitor_id: 'enc_AbC123',
      visitor_id: '550e8400-e29b-41d4-a716-446655440000',
      success: true,
      message: 'Visitor enriched successfully',
      set_keys: ['user_id'],
      unset_keys: ['trial_ends_at'],
    })
  })

  test('carries success:false through verbatim instead of rewriting it', () => {
    const notFound: EnrichVisitorResponse = {
      success: false,
      visitor_id: 'enc_AbC123',
      message: 'Visitor not found',
    }
    const output = enrichJsonOutput('enc_AbC123', notFound, {
      setKeys: ['user_id'],
      unsetKeys: [],
    })
    expect(output.success).toBe(false)
    expect(output.message).toBe('Visitor not found')
  })

  test('never carries the enrichment values, only the key names', () => {
    const output = enrichJsonOutput('enc_AbC123', response, {
      setKeys: ['email'],
      unsetKeys: [],
    })
    expect(JSON.stringify(output)).not.toContain('ada@example.com')
  })
})

describe('visitorNotFoundMessage', () => {
  test('keeps the server message and says nothing was written', () => {
    const message = visitorNotFoundMessage('enc_AbC123', 'Visitor not found')
    expect(message).toContain('Visitor not found')
    expect(message).toContain('enc_AbC123')
    expect(message).toContain('nothing was enriched')
  })

  test('tells the operator where a sealed ID comes from and what to check', () => {
    const message = visitorNotFoundMessage('enc_AbC123', 'Visitor not found')
    expect(message).toContain('_temps_visitor_id')
    expect(message).toContain('project')
  })
})
