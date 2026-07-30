import { describe, expect, test } from 'bun:test'
import {
  createCredentialRevealGuard,
  credentialValueForScope,
} from './credential-reveal-state'

describe('credentialValueForScope', () => {
  const revealed = {
    value: 'old-secret',
    scope: 'service-17:version-1',
  }

  test('returns a reveal only while its resource version is current', () => {
    expect(credentialValueForScope(revealed, 'service-17:version-1')).toBe(
      'old-secret'
    )
  })

  test('hides a revealed value after the credential resource is refreshed', () => {
    expect(
      credentialValueForScope(revealed, 'service-17:version-2')
    ).toBeUndefined()
  })

  test('never carries a reveal to another resource with the same field name', () => {
    expect(
      credentialValueForScope(revealed, 'service-18:version-1')
    ).toBeUndefined()
  })

  test('does not revive plaintext after navigating A to B and back to A', () => {
    const firstVisit = {
      value: 'service-a-secret',
      scope: 'service-a:navigation-1:version-1',
    }

    expect(
      credentialValueForScope(
        firstVisit,
        'service-a:navigation-3:version-1'
      )
    ).toBeUndefined()
  })
})

describe('createCredentialRevealGuard', () => {
  test('discards an in-flight response after switching resources', () => {
    const guard = createCredentialRevealGuard()
    const request = guard.begin('password')

    guard.invalidate()

    expect(guard.isCurrent('password', request)).toBe(false)
    expect(guard.finish('password', request)).toBe(false)
  })

  test('discards an older overlapping response for the same field', () => {
    const guard = createCredentialRevealGuard()
    const olderRequest = guard.begin('url')
    const newerRequest = guard.begin('url')

    expect(guard.isCurrent('url', olderRequest)).toBe(false)
    expect(guard.isCurrent('url', newerRequest)).toBe(true)
  })

  test('allows independent fields to reveal concurrently', () => {
    const guard = createCredentialRevealGuard()
    const urlRequest = guard.begin('url')
    const tokenRequest = guard.begin('env.API_TOKEN')

    expect(guard.isCurrent('url', urlRequest)).toBe(true)
    expect(guard.isCurrent('env.API_TOKEN', tokenRequest)).toBe(true)
  })

  test('discards an in-flight response after the field is hidden', () => {
    const guard = createCredentialRevealGuard()
    const request = guard.begin('DATABASE_URL')

    guard.cancel('DATABASE_URL')

    expect(guard.isCurrent('DATABASE_URL', request)).toBe(false)
    expect(guard.finish('DATABASE_URL', request)).toBe(false)
  })
})
