import { test, expect, describe } from 'bun:test'
import { resolveDimension, DIMENSION_MAP } from './breakdown.js'

describe('resolveDimension', () => {
  test('resolves every documented dimension to a groupBy/label pair', () => {
    for (const key of Object.keys(DIMENSION_MAP)) {
      expect(resolveDimension(key)).toEqual(DIMENSION_MAP[key]!)
    }
  })

  test('rejects an unknown dimension and lists the valid ones', () => {
    expect(() => resolveDimension('countryz')).toThrow(/Unknown dimension "countryz"/)
    expect(() => resolveDimension('countryz')).toThrow(/pages, referrers/)
  })

  test('is case-sensitive: uppercase does not silently match', () => {
    // Sending the resolved groupBy is what reaches the API; a case-insensitive
    // match here would mean --top Pages either silently "worked" via a typo or
    // resolved to the wrong dimension.
    expect(() => resolveDimension('Pages')).toThrow(/Unknown dimension "Pages"/)
  })
})
