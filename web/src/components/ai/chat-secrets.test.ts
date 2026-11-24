// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  isRequestSecretTool,
  isSecretReference,
  parseSecretRequest,
  secretRequestState,
} from './chat-secrets'

const ID = '0123456789abcdef0123456789abcdef'

const OTHER = 'fedcba9876543210fedcba9876543210'

describe('parseSecretRequest', () => {
  test('reads every input of the form the tool returned', () => {
    const request = parseSecretRequest(
      JSON.stringify({
        status: 'awaiting_user',
        operation_id: 'create_s3_source',
        operation_summary: 'Create a new S3 source',
        purpose: 'Register the bucket for backups',
        requests: [
          {
            handle: `secret_ref:v1:${ID}`,
            field: '/access_key_id',
            label: 'Access key ID',
            help: null,
            status: 'awaiting',
          },
          {
            handle: `secret_ref:v1:${OTHER}`,
            field: '/secret_key',
            label: 'Secret access key',
            help: 'Shown once by your provider',
            status: 'awaiting',
          },
        ],
      })
    )
    expect(request).toEqual({
      operationId: 'create_s3_source',
      operationSummary: 'Create a new S3 source',
      purpose: 'Register the bucket for backups',
      items: [
        { id: ID, label: 'Access key ID', field: 'access_key_id', help: null },
        {
          id: OTHER,
          label: 'Secret access key',
          field: 'secret_key',
          help: 'Shown once by your provider',
        },
      ],
    })
  })

  test('skips an entry without a usable handle but keeps the rest', () => {
    const request = parseSecretRequest(
      JSON.stringify({
        requests: [
          { handle: 'not-a-handle', label: 'x' },
          { handle: 'secret_ref:v1:', label: 'y' },
          { handle: `secret_ref:v1:${ID}`, label: 'Token' },
        ],
      })
    )
    expect(request?.items.map((item) => item.id)).toEqual([ID])
  })

  test('is null while the call has no result yet', () => {
    expect(parseSecretRequest(null)).toBeNull()
    expect(parseSecretRequest(undefined)).toBeNull()
    expect(parseSecretRequest('')).toBeNull()
  })

  test('is null for a refusal sentence or a descriptor without handles', () => {
    expect(parseSecretRequest("'x' takes no credential fields")).toBeNull()
    expect(parseSecretRequest('{"status":"awaiting_user"}')).toBeNull()
    expect(parseSecretRequest('{"requests":[]}')).toBeNull()
    expect(parseSecretRequest('{"requests":[{"handle":"nope"}]}')).toBeNull()
    expect(parseSecretRequest('null')).toBeNull()
  })
})

describe('isRequestSecretTool', () => {
  test('matches the bare and the MCP-prefixed tool name', () => {
    expect(isRequestSecretTool('request_secret')).toBe(true)
    expect(isRequestSecretTool('mcp__temps-chat__request_secret')).toBe(true)
    expect(isRequestSecretTool('temps_write')).toBe(false)
  })
})

describe('isSecretReference', () => {
  test('recognises a handle and nothing else', () => {
    expect(isSecretReference(`secret_ref:v1:${ID}`)).toBe(true)
    expect(isSecretReference('***')).toBe(false)
    expect(isSecretReference(42)).toBe(false)
    expect(isSecretReference(undefined)).toBe(false)
  })
})

describe('secretRequestState', () => {
  const open = [
    { id: ID, status: 'awaiting' },
    { id: 'b', status: 'filled' },
  ]
  test('follows the open list', () => {
    expect(secretRequestState(ID, open)).toBe('awaiting')
    expect(secretRequestState('b', open)).toBe('filled')
  })
  test('a request that left the list is closed, not pending', () => {
    expect(secretRequestState('gone', open)).toBe('closed')
  })
  test('is unknown until the list has loaded', () => {
    expect(secretRequestState(ID, undefined)).toBe('unknown')
  })
})
