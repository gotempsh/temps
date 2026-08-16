import { test, expect, describe } from 'bun:test'
import { emailStatusVariant, sanitizeEmailBody, buildEmailListQuery } from './index.js'

describe('emailStatusVariant', () => {
  test('maps delivered and sent to active', () => {
    expect(emailStatusVariant('delivered')).toBe('active')
    expect(emailStatusVariant('sent')).toBe('active')
  })

  test('maps failed to error', () => {
    expect(emailStatusVariant('failed')).toBe('error')
  })

  test('falls back to pending for anything else', () => {
    expect(emailStatusVariant('queued')).toBe('pending')
    expect(emailStatusVariant('unknown-status')).toBe('pending')
  })
})

describe('sanitizeEmailBody', () => {
  test('strips control and escape characters from attacker-controlled bodies', () => {
    // Email bodies come from the send API and are rendered straight to the
    // terminal — an unescaped \x1b[ could rewrite the prompt or hide output.
    const malicious = 'Hello\x1b[31m RED \x1b[0m\x07World'
    const cleaned = sanitizeEmailBody(malicious)
    expect(cleaned).not.toContain('\x1b')
    expect(cleaned).not.toContain('\x07')
    expect(cleaned).toBe('Hello[31m RED [0mWorld')
  })

  test('leaves normal text and newlines untouched', () => {
    const body = 'Hi there,\nThanks for signing up!'
    expect(sanitizeEmailBody(body)).toBe(body)
  })
})

describe('buildEmailListQuery', () => {
  test('returns an empty object when no filters were given', () => {
    expect(buildEmailListQuery({})).toEqual({})
  })

  test('parses numeric fields and passes through string fields', () => {
    expect(buildEmailListQuery({
      page: '2',
      pageSize: '50',
      status: 'sent',
      domainId: '7',
      projectId: '3',
      fromAddress: 'noreply@example.com',
    })).toEqual({
      page: 2,
      page_size: 50,
      status: 'sent',
      domain_id: 7,
      project_id: 3,
      from_address: 'noreply@example.com',
    })
  })

  test('drops zero-valued numeric filters instead of sending them', () => {
    // page/pageSize/domainId/projectId are gated with `page && {...}`, so a
    // literal "0" is falsy and silently omitted rather than sent as 0. An
    // agent scripting `--page 0` would get page 1's results back, not an error.
    expect(buildEmailListQuery({ page: '0', pageSize: '0', domainId: '0', projectId: '0' })).toEqual({})
  })
})
