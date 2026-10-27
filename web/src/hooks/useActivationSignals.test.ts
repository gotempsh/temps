// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { appsReachableSignal } from './useActivationSignals'

test('apps are reachable through a usable preview domain or a tunnel', () => {
  expect(
    appsReachableSignal(
      true,
      { status: 'usable', tunnel_environment_hosts: null },
      false
    )
  ).toBe(true)
  expect(
    appsReachableSignal(
      true,
      { status: 'local_only', tunnel_environment_hosts: 'edge' },
      false
    )
  ).toBe(true)
  expect(
    appsReachableSignal(
      true,
      { status: 'local_only', tunnel_environment_hosts: null },
      true
    )
  ).toBe(false)
  expect(appsReachableSignal(true, undefined, true)).toBe(false)
})

test('who cannot read the preview domain status keeps the wildcard signal', () => {
  expect(appsReachableSignal(false, undefined, true)).toBe(true)
  expect(appsReachableSignal(false, undefined, false)).toBe(false)
})
