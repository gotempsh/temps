// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { PluginGrantFields } from './PluginGrantFields'
import { emptyPluginGrants, pluginPermissionValues } from '@/lib/plugin-grants'

test('permissions default to unchecked shared controls with explicit scope', () => {
  const markup = renderToStaticMarkup(
    <PluginGrantFields value={emptyPluginGrants()} onChange={() => {}} />
  )
  expect(markup.match(/aria-checked="false"/g)).toHaveLength(
    pluginPermissionValues.length
  )
  expect(markup).toContain('do not sandbox native plugin code')
  expect(markup).toContain('across this instance')
  expect(markup).not.toContain('AI calls per day')
})

test('AI approval exposes limits and distinguishes call limits from a money budget', () => {
  const markup = renderToStaticMarkup(
    <PluginGrantFields
      value={{ ...emptyPluginGrants(), permissions: ['ai_generate'] }}
      onChange={() => {}}
    />
  )
  expect(markup).toContain('AI calls per day')
  expect(markup).toContain('Maximum output tokens per call')
  expect(markup).toContain('not a currency budget')
})

test('undeclared permissions are explained and unavailable', () => {
  const markup = renderToStaticMarkup(
    <PluginGrantFields
      value={emptyPluginGrants()}
      requested={['ai_generate']}
      onChange={() => {}}
    />
  )
  expect(markup.match(/Not requested by this plugin/g)).toHaveLength(
    pluginPermissionValues.length - 1
  )
  expect(markup).toContain('disabled')
})
