// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { pluginAuditActor } from './plugin-audit-actor'

test('shows stable plugin identity for host-authored audit operations', () => {
  expect(
    pluginAuditActor('EXTERNAL_PLUGIN_HOST_CALL_SUCCEEDED', {
      actor: { kind: 'plugin', id: 'actor-id', name: 'example' },
    })
  ).toEqual({ id: 'actor-id', name: 'example' })
})

test.each([
  undefined,
  {},
  { actor: null },
  { actor: [] },
  { actor: { kind: 'user', id: '1', name: 'example' } },
  { actor: { kind: 'plugin', id: 1, name: 'example' } },
])('ignores missing or malformed identity %j', (data) => {
  expect(
    pluginAuditActor('EXTERNAL_PLUGIN_HOST_CALL_SUCCEEDED', data)
  ).toBeNull()
})

test('does not reinterpret arbitrary audit data as plugin attribution', () => {
  expect(
    pluginAuditActor('PROJECT_CREATED', {
      actor: { kind: 'plugin', id: 'actor-id', name: 'example' },
    })
  ).toBeNull()
})
