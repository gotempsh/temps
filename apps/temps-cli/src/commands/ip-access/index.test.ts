import { test, expect, describe } from 'bun:test'
import { resolveIpAccessAction } from './index.js'

describe('resolveIpAccessAction', () => {
  test('passes allow through untouched', () => {
    expect(resolveIpAccessAction('allow')).toBe('allow')
  })

  test('normalizes deny to block for the API', () => {
    expect(resolveIpAccessAction('deny')).toBe('block')
  })

  test('passes block through untouched', () => {
    expect(resolveIpAccessAction('block')).toBe('block')
  })

  test('rejects an unknown action instead of sending it to the API', () => {
    expect(resolveIpAccessAction('reject')).toBeUndefined()
  })

  test('rejects an empty string', () => {
    expect(resolveIpAccessAction('')).toBeUndefined()
  })

  test('is case-sensitive: "Allow" is not normalized silently', () => {
    // A silently-passed miscased value would reach the API as an unrecognized
    // action instead of the clear CLI warning the user should see.
    expect(resolveIpAccessAction('Allow')).toBeUndefined()
  })
})
