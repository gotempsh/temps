// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { isValidCommitSha, redeployGitReference } from './redeploy-reference'

const SHA = '3f9a1c2b7d4e5f60718293a4b5c6d7e8f9012345'

describe('redeployGitReference', () => {
  test('a branch deployment is redeployed at its own commit, not the branch head', () => {
    expect(
      redeployGitReference({ type: 'branch', branch: 'main', commit: SHA })
    ).toEqual({ branch: 'main', commit: SHA })
  })

  test('a tag deployment keeps the tag and pins its commit', () => {
    expect(
      redeployGitReference({ type: 'tag', tag: 'v1.2.0', commit: SHA })
    ).toEqual({ tag: 'v1.2.0', commit: SHA })
  })

  test('a commit deployment sends the commit', () => {
    expect(redeployGitReference({ type: 'commit', commit: SHA })).toEqual({
      commit: SHA,
    })
  })

  test('the stored commit is normalised', () => {
    expect(
      redeployGitReference({
        type: 'branch',
        branch: ' main ',
        commit: ` ${SHA.toUpperCase()} `,
      })
    ).toEqual({ branch: 'main', commit: SHA })
  })

  test('a placeholder or missing commit falls back to the branch', () => {
    // The server stores `manual-trigger-<ts>` when it could not resolve a
    // commit; that cannot be checked out, so the branch is all there is.
    for (const commit of ['manual-trigger-1760000000', '', null, undefined]) {
      expect(
        redeployGitReference({ type: 'branch', branch: 'main', commit })
      ).toEqual({ branch: 'main', commit: undefined })
    }
  })
})

test('a commit deployment without a usable commit cannot be redeployed', () => {
  // Sending nothing would make the server build the default branch instead.
  for (const commit of ['manual-trigger-1760000000', '', null]) {
    expect(redeployGitReference({ type: 'commit', commit })).toBeNull()
  }
})

describe('isValidCommitSha', () => {
  test('accepts 7 to 40 hex characters only', () => {
    expect(isValidCommitSha('3f9a1c2')).toBe(true)
    expect(isValidCommitSha(SHA)).toBe(true)
    expect(isValidCommitSha('3f9a1c')).toBe(false)
    expect(isValidCommitSha('manual-trigger-1')).toBe(false)
  })
})
