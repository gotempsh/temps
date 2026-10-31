// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { isMfaChallengeEnded } from './mfaChallengeProblem'

describe('isMfaChallengeEnded', () => {
  it('recognizes generated-client errors with flat extensions', () => {
    expect(isMfaChallengeEnded({ error_code: 'MFA_CHALLENGE_ENDED' })).toBe(
      true
    )
  })

  it('recognizes nested RFC 7807 extensions', () => {
    expect(
      isMfaChallengeEnded({ extensions: { error_code: 'MFA_CHALLENGE_ENDED' } })
    ).toBe(true)
  })

  it('keeps a wrong code or a lockout on the verification page', () => {
    expect(isMfaChallengeEnded({ title: 'MFA verification failed' })).toBe(
      false
    )
    expect(isMfaChallengeEnded({ error_code: 'MFA_LOCKED' })).toBe(false)
    expect(isMfaChallengeEnded(new Error('network failure'))).toBe(false)
  })
})
