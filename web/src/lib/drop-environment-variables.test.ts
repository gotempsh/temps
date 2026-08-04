import { describe, expect, test } from 'bun:test'
import {
  serializeDropEnvironmentVariables,
  validateDropEnvironmentVariables,
} from './drop-environment-variables'

const variable = (key: string, value = '') => ({ id: key, key, value })

describe('drop environment variables', () => {
  test('accepts empty values and serializes trimmed keys', () => {
    const variables = [
      variable(' API_URL ', 'https://example.com'),
      variable('EMPTY'),
    ]

    expect(validateDropEnvironmentVariables(variables)).toBeNull()
    expect(serializeDropEnvironmentVariables(variables)).toEqual([
      ['API_URL', 'https://example.com'],
      ['EMPTY', ''],
    ])
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
})
