// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { resolveBitbucketAuthNonInteractive } from './index.js'

describe('resolveBitbucketAuthNonInteractive', () => {
  test('uses --password when --username and --password are both given', () => {
    expect(
      resolveBitbucketAuthNonInteractive({ username: 'dev', password: 'pw', token: 'tok' }),
    ).toEqual({ type: 'app_password', username: 'dev', password: 'pw' })
  })

  test('falls back to --token as the app password when --password is omitted', () => {
    expect(
      resolveBitbucketAuthNonInteractive({ username: 'dev', token: 'tok' }),
    ).toEqual({ type: 'app_password', username: 'dev', password: 'tok' })
  })

  test('rejects --username with neither --password nor --token', () => {
    // Without this guard the CLI would send an app_password auth body with
    // an empty password string instead of failing before the request.
    expect(() => resolveBitbucketAuthNonInteractive({ username: 'dev' })).toThrow(
      /Bitbucket app password is required/,
    )
  })

  test('uses an access token when --username is not given', () => {
    expect(resolveBitbucketAuthNonInteractive({ token: 'tok' })).toEqual({
      type: 'access_token',
      token: 'tok',
    })
  })

  test('rejects a call with neither --username nor --token', () => {
    expect(() => resolveBitbucketAuthNonInteractive({})).toThrow(
      /Bitbucket access token is required/,
    )
  })
})
