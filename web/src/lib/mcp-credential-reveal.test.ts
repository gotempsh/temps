import { describe, expect, test } from 'bun:test'
import {
  listMaskedMcpCredentialFields,
  MASKED_CREDENTIAL_VALUE,
  replaceMcpCredentialValue,
} from './mcp-credential-reveal'

describe('listMaskedMcpCredentialFields', () => {
  test('finds masked URL, environment variables, and headers', () => {
    const config = JSON.stringify({
      url: MASKED_CREDENTIAL_VALUE,
      env: { API_TOKEN: MASKED_CREDENTIAL_VALUE, PUBLIC_MODE: 'safe' },
      headers: {
        Authorization: MASKED_CREDENTIAL_VALUE,
        Accept: 'application/json',
      },
    })

    expect(listMaskedMcpCredentialFields(config)).toEqual([
      'env.API_TOKEN',
      'headers.Authorization',
      'url',
    ])
  })

  test('fails closed for malformed or non-object JSON', () => {
    expect(listMaskedMcpCredentialFields('{')).toEqual([])
    expect(listMaskedMcpCredentialFields('[]')).toEqual([])
  })
})

describe('replaceMcpCredentialValue', () => {
  test('replaces only the selected environment credential', () => {
    const result = JSON.parse(
      replaceMcpCredentialValue(
        JSON.stringify({
          env: {
            API_TOKEN: MASKED_CREDENTIAL_VALUE,
            SECOND_TOKEN: MASKED_CREDENTIAL_VALUE,
          },
        }),
        'env.API_TOKEN',
        'secret'
      )
    )

    expect(result.env).toEqual({
      API_TOKEN: 'secret',
      SECOND_TOKEN: MASKED_CREDENTIAL_VALUE,
    })
  })

  test('preserves dots in header names', () => {
    const result = JSON.parse(
      replaceMcpCredentialValue(
        JSON.stringify({
          headers: { 'X.Auth.Token': MASKED_CREDENTIAL_VALUE },
        }),
        'headers.X.Auth.Token',
        'secret'
      )
    )

    expect(result.headers['X.Auth.Token']).toBe('secret')
  })

  test('rejects fields outside the reveal API allowlist', () => {
    expect(() =>
      replaceMcpCredentialValue(
        JSON.stringify({ command: MASKED_CREDENTIAL_VALUE }),
        'command',
        'secret'
      )
    ).toThrow('Unsupported MCP credential field')
  })
})
