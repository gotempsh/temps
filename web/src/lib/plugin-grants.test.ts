// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import {
  emptyPluginGrants,
  pluginGrantErrors,
  pluginGrantsBody,
  pluginGrantsSchema,
  pluginOutboundLabel,
} from './plugin-grants'

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
  { write_areas: ['databases'] },
  { write_project_ids: [0] },
  { write_project_ids: [1.5] },
  { write_project_ids: Array.from({ length: 101 }, (_, index) => index + 1) },
  // Limited to some services, with none chosen.
  { permissions: ['api_write'], write_project_ids: [] },
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

test('a new permission to change reaches nothing until an area is chosen', () => {
  const grants = emptyPluginGrants()
  expect(grants.write_areas).toEqual([])
  expect(grants.write_project_ids).toBeNull()
  expect(
    pluginGrantsSchema.safeParse({ ...grants, permissions: ['api_write'] })
      .success
  ).toBe(true)
})

test('a limit to some services is accepted with at least one and said when empty', () => {
  const limited = {
    ...emptyPluginGrants(),
    permissions: ['api_write'],
    write_areas: ['deployments'],
  }
  expect(
    pluginGrantsSchema.safeParse({ ...limited, write_project_ids: [7] }).success
  ).toBe(true)
  const empty = pluginGrantsSchema.safeParse({
    ...limited,
    write_project_ids: [],
  })
  expect(empty.success).toBe(false)
  expect(empty.error?.issues[0]).toMatchObject({
    path: ['write_project_ids'],
    message:
      'Choose at least one service, or let the plugin change every service.',
  })
})

test('where a plugin may change things is not sent without the permission to change', () => {
  const stale = {
    ...emptyPluginGrants(),
    permissions: ['api_read' as const],
    write_areas: ['deployments' as const],
    write_project_ids: [7],
  }
  expect(pluginGrantsBody(stale)).toMatchObject({
    write_areas: [],
    write_project_ids: null,
  })
  const kept = { ...stale, permissions: ['api_write' as const] }
  expect(pluginGrantsBody(kept)).toBe(kept)
})

test('every validation message is shown once, whatever field it belongs to', () => {
  expect(pluginGrantErrors(undefined)).toEqual([])
  expect(
    pluginGrantErrors({
      ai_daily_call_limit: { message: 'Choose 0 to 10,000 AI calls per day.' },
      write_project_ids: {
        message: 'Choose at least one service.',
        ref: { message: 'not a message' },
      },
      write_areas: [{ message: 'Choose 0 to 10,000 AI calls per day.' }],
    })
  ).toEqual([
    'Choose 0 to 10,000 AI calls per day.',
    'Choose at least one service.',
  ])
  const parsed = pluginGrantsSchema.safeParse({
    ...emptyPluginGrants(),
    ai_daily_call_limit: 20000,
  })
  expect(parsed.error?.issues[0].message).toBe(
    'Choose 0 to 10,000 AI calls per day.'
  )
})

test('the outbound profile of a plugin has a plain label', () => {
  expect(pluginOutboundLabel('open')).toEqual({
    label: 'Unrestricted',
    tone: 'open',
  })
  expect(pluginOutboundLabel('restricted').tone).toBe('limited')
  expect(pluginOutboundLabel('none').label).toBe('No outbound access')
  // A profile this console does not know is not presented as closed.
  expect(pluginOutboundLabel('future').tone).toBe('open')
})
