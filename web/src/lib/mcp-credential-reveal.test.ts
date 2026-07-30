import { describe, expect, test } from 'bun:test'
import {
  listMaskedMcpCredentialFields,
  MASKED_CREDENTIAL_VALUE,
  revealMcpCredentialValueIfStillMasked,
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

describe('revealMcpCredentialValueIfStillMasked', () => {
  test('preserves unrelated edits made while a reveal is in flight', () => {
    const result = revealMcpCredentialValueIfStillMasked(
      JSON.stringify({
        url: MASKED_CREDENTIAL_VALUE,
        env: { MODE: 'edited-during-reveal' },
      }),
      'url',
      'https://user:secret@example.test'
    )

    expect(JSON.parse(result!)).toEqual({
      url: 'https://user:secret@example.test',
      env: { MODE: 'edited-during-reveal' },
    })
  })

  test('discards a reveal after its field is deleted or renamed', () => {
    expect(
      revealMcpCredentialValueIfStillMasked(
        JSON.stringify({ env: { RENAMED_TOKEN: MASKED_CREDENTIAL_VALUE } }),
        'env.API_TOKEN',
        'secret'
      )
    ).toBeUndefined()
  })

  test('discards a reveal after the user replaces the sentinel', () => {
    expect(
      revealMcpCredentialValueIfStillMasked(
        JSON.stringify({ env: { API_TOKEN: 'new-user-value' } }),
        'env.API_TOKEN',
        'old-secret'
      )
    ).toBeUndefined()
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
