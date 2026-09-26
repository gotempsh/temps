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
  expect(markup).toContain('Permission requirements are not published')
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

test('known requirements show only requested access with explicit required and optional labels', () => {
  const markup = renderToStaticMarkup(
    <PluginGrantFields
      value={emptyPluginGrants()}
      onChange={() => {}}
      requirements={[
        {
          permission: 'projects_read',
          required: true,
          reason: 'Lists projects for the core workflow.',
        },
        {
          permission: 'events_read',
          required: false,
          reason: 'Automatic scans; manual scans work without it.',
        },
      ]}
    />
  )
  expect(markup.match(/aria-checked="false"/g)).toHaveLength(2)
  expect(markup).toContain('Required')
  expect(markup).toContain('Optional')
  expect(markup).toContain('Automatic scans; manual scans work without it.')
  expect(markup).not.toContain('Use AI')
})

test('explicitly empty requirements mean no host permissions, not unknown requirements', () => {
  const markup = renderToStaticMarkup(
    <PluginGrantFields
      value={emptyPluginGrants()}
      onChange={() => {}}
      requirements={[]}
    />
  )
  expect(markup).toContain('No host API permissions requested')
  expect(markup).not.toContain('role="checkbox"')
  expect(markup).not.toContain('not published')
})
