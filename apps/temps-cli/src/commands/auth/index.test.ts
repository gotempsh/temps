import { test, expect, describe } from 'bun:test'
import { resolveLoginUrl } from './index.js'

describe('resolveLoginUrl', () => {
  test('positional url wins over --url', () => {
    expect(resolveLoginUrl('https://positional.example.com', { url: 'https://flag.example.com' })).toBe(
      'https://positional.example.com',
    )
  })

  test('falls back to --url when no positional arg was given', () => {
    expect(resolveLoginUrl(undefined, { url: 'https://flag.example.com' })).toBe(
      'https://flag.example.com',
    )
  })

  test('an empty positional does not shadow --url', () => {
    // `temps login ""` from a shell glob/variable expansion shouldn't win
    // over an explicit --url.
    expect(resolveLoginUrl('', { url: 'https://flag.example.com' })).toBe(
      'https://flag.example.com',
    )
  })

  test('returns undefined when neither is given', () => {
    expect(resolveLoginUrl(undefined, {})).toBeUndefined()
  })
})
