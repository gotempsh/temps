// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { emptyPluginGrants, pluginGrantsSchema } from './plugin-grants'

test('new grants deny all host access and defaults are independent', () => {
  const first = emptyPluginGrants()
  first.permissions.push('ai_generate')
  expect(emptyPluginGrants().permissions).toEqual([])
})

test.each([
  { permissions: ['system_admin'] },
  { ai_daily_call_limit: -1 },
  { ai_daily_call_limit: 10001 },
  { ai_daily_call_limit: 1.5 },
  { ai_max_output_tokens: 0 },
  { ai_max_output_tokens: 4097 },
  { ai_max_output_tokens: Number.NaN },
])('rejects invalid grant configuration %j', (override) => {
  expect(
    pluginGrantsSchema.safeParse({ ...emptyPluginGrants(), ...override })
      .success
  ).toBe(false)
})

test('zero daily calls is an explicit pause', () => {
  expect(
    pluginGrantsSchema.parse({ ...emptyPluginGrants(), ai_daily_call_limit: 0 })
      .ai_daily_call_limit
  ).toBe(0)
})
