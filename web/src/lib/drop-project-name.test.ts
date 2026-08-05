import { describe, expect, test } from 'bun:test'
import {
  ensureDropProjectName,
  normalizeDropProjectName,
} from './drop-project-name'

describe('Drop project names', () => {
  test('slugifies spaces, punctuation, casing, and accents', () => {
    expect(normalizeDropProjectName('My Café Site!')).toBe('my-cafe-site')
  })

  test('truncates names to the project DNS-label budget', () => {
    const result = normalizeDropProjectName(`${'very-long-'.repeat(8)}site`)
    expect(result.length).toBeLessThanOrEqual(40)
    expect(result.endsWith('-')).toBe(false)
  })

  test('generates a stable-shaped fallback when no usable name exists', () => {
    expect(ensureDropProjectName('🔥🔥', () => 'a1b2c3d4')).toBe(
      'drop-a1b2c3d4'
    )
  })

  test('does not add randomness to a usable name', () => {
    expect(ensureDropProjectName('Release Candidate', () => 'unused')).toBe(
      'release-candidate'
    )
  })
})
