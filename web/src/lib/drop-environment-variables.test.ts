import { describe, expect, test } from 'bun:test'
import {
  serializeDropEnvironmentVariables,
  validateDropEnvironmentVariables,
} from './drop-environment-variables'

const variable = (key: string, value = '', isSecret = false) => ({
  id: key,
  key,
  value,
  isSecret,
})

describe('drop environment variables', () => {
  test('accepts empty values and serializes trimmed keys', () => {
    const variables = [
      variable(' API_URL ', 'https://example.com'),
      variable('EMPTY'),
    ]

    expect(validateDropEnvironmentVariables(variables)).toBeNull()
    expect(serializeDropEnvironmentVariables(variables)).toEqual([
      { key: 'API_URL', value: 'https://example.com', is_secret: false },
      { key: 'EMPTY', value: '', is_secret: false },
    ])
  })

  test('carries the secret flag through serialization', () => {
    expect(
      serializeDropEnvironmentVariables([variable('TOKEN', 'sk-live', true)])
    ).toEqual([{ key: 'TOKEN', value: 'sk-live', is_secret: true }])
  })

  test('rejects missing, invalid, and duplicate keys', () => {
    expect(validateDropEnvironmentVariables([variable('')])).toContain(
      'needs a key'
    )
    expect(validateDropEnvironmentVariables([variable('1INVALID')])).toContain(
      'not a valid'
    )
    expect(
      validateDropEnvironmentVariables([
        variable('API_URL'),
        variable('API_URL'),
      ])
    ).toContain('more than once')
  })

  test('rejects a secret with no value', () => {
    // Write-only means it could never be filled in afterwards.
    expect(
      validateDropEnvironmentVariables([variable('TOKEN', '', true)])
    ).toContain('marked as a secret but has no value')
  })
})
