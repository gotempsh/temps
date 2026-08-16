import { describe, expect, test } from 'bun:test'

import { withComposeSandboxDisabled } from './compose-security-settings'

describe('withComposeSandboxDisabled', () => {
  test('atomically removes elevated capabilities when disabling the sandbox', () => {
    expect(
      withComposeSandboxDisabled(['db', 'webserver'], [], 'webserver', true)
    ).toEqual({
      relaxedCapabilityServices: ['db'],
      unsandboxedServices: ['webserver'],
    })
  })

  test('restores the sandbox without changing other capability choices', () => {
    expect(
      withComposeSandboxDisabled(
        ['db'],
        ['webserver', 'worker'],
        'webserver',
        false
      )
    ).toEqual({
      relaxedCapabilityServices: ['db'],
      unsandboxedServices: ['worker'],
    })
  })

  test('does not add duplicate service names', () => {
    expect(
      withComposeSandboxDisabled([], ['webserver'], 'webserver', true)
        .unsandboxedServices
    ).toEqual(['webserver'])
  })
})
