// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { AUTOMATIC_LISTEN, ingressSaveBody } from './worker-ingress-save'

test('a save carries the revision the settings were read with', () => {
  expect(
    ingressSaveBody(
      {
        enabled: true,
        listen: AUTOMATIC_LISTEN,
        publicAddress: ' 203.0.113.7 ',
        httpPort: '8080',
        httpsPort: '',
      },
      '9f2c4a1b0d3e5f67'
    )
  ).toEqual({
    enabled: true,
    listen_address: null,
    public_address: '203.0.113.7',
    http_port: 8080,
    https_port: 443,
    expected_revision: '9f2c4a1b0d3e5f67',
  })
})

test('before the settings are read the save is unconditional', () => {
  expect(
    ingressSaveBody(
      {
        enabled: false,
        listen: '10.0.0.5',
        publicAddress: '',
        httpPort: '80',
        httpsPort: '443',
      },
      undefined
    ).expected_revision
  ).toBeNull()
})
