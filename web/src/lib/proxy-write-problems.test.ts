// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  confirmationFlag,
  ipRuleConfirmation,
  routeFieldProblem,
} from './proxy-write-problems'

test('a refused route domain is shown on the domain field', () => {
  for (const title of ['Invalid Route Domain', 'Domain In Use']) {
    expect(
      routeFieldProblem({ title, status: 400, detail: 'Pick another name.' })
    ).toEqual({ field: 'domain', message: 'Pick another name.' })
  }
})

test('a refused upstream is shown on the host field', () => {
  expect(
    routeFieldProblem({
      title: 'Invalid Route Upstream',
      status: 400,
      detail: "route upstream '127.0.0.1' is not an allowed destination",
    })
  ).toEqual({
    field: 'host',
    message: "route upstream '127.0.0.1' is not an allowed destination",
  })
})

test('a refusal that is not about one field keeps the generic failure', () => {
  expect(
    routeFieldProblem({ title: 'Forbidden', status: 403, detail: 'no' })
  ).toBeNull()
  expect(routeFieldProblem(new Error('network down'))).toBeNull()
  expect(routeFieldProblem(undefined)).toBeNull()
})

test('only the two confirmable IP rule conflicts ask for confirmation', () => {
  expect(
    ipRuleConfirmation({
      title: 'Block Range Needs Confirmation',
      status: 409,
      detail: 'x',
    })
  ).toBe('broad_range')
  expect(
    ipRuleConfirmation({
      title: 'Rule Would Lock You Out',
      status: 409,
      detail: 'x',
    })
  ).toBe('lockout')
  // A duplicate address is also a 409 and must not offer an override.
  expect(
    ipRuleConfirmation({
      title: 'Duplicate IP Address',
      status: 409,
      detail: 'x',
    })
  ).toBeNull()
  // The title alone is not enough: the status must be the 409 the server sends.
  expect(
    ipRuleConfirmation({
      title: 'Rule Would Lock You Out',
      status: 500,
      detail: 'x',
    })
  ).toBeNull()
})

test('each confirmation answers with its own request flag', () => {
  expect(confirmationFlag('broad_range')).toBe('confirm_broad_range')
  expect(confirmationFlag('lockout')).toBe('acknowledge_lockout')
})
