import { describe, expect, test } from 'bun:test'
import { credentialValueForScope } from './credential-reveal-state'

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
})
