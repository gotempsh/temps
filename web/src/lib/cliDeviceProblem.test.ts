import { describe, expect, it } from 'bun:test'
import { cliDeviceLookupErrorKind } from './cliDeviceProblem'

describe('cliDeviceLookupErrorKind', () => {
  it('recognizes generated-client errors with flat extensions', () => {
    expect(
      cliDeviceLookupErrorKind({
        error_code: 'CLI_DEVICE_SESSION_NOT_FOUND',
      })
    ).toBe('not_found')
  })

  it('recognizes nested RFC 7807 extensions', () => {
    expect(
      cliDeviceLookupErrorKind({
        extensions: { error_code: 'CLI_DEVICE_SESSION_EXPIRED' },
      })
    ).toBe('expired')
  })

  it('does not classify unrelated errors', () => {
    expect(cliDeviceLookupErrorKind(new Error('network failure'))).toBeNull()
  })
})
