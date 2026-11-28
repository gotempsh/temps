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

test('like an app permission screen, only what the plugin asks for is listed', () => {
  const markup = renderToStaticMarkup(
    <PluginGrantFields
      value={emptyPluginGrants()}
      requested={['ai_generate', 'api_write']}
      onChange={() => {}}
    />
  )
  expect(markup.match(/role="checkbox"/g)).toHaveLength(2)
  expect(markup).toContain('Use AI')
  expect(markup).toContain('Change things in Temps')
  expect(markup).not.toContain('Read deployments')
  expect(markup).toContain('This plugin asks for the access listed here')
  expect(markup).toContain('Nothing is granted by default')
})

test('a permission the plugin stopped asking for stays listed so it can be taken away', () => {
  const markup = renderToStaticMarkup(
    <PluginGrantFields
      value={{ ...emptyPluginGrants(), permissions: ['deployments_read'] }}
      requested={['ai_generate']}
      onChange={() => {}}
    />
  )
  expect(markup.match(/role="checkbox"/g)).toHaveLength(2)
  expect(markup).toContain('Read deployments')
  expect(markup).toContain('No longer requested by this plugin')
  // It can be unchecked, not checked again.
  expect(markup).toContain('aria-checked="true"')
})

test('a plugin that asks for nothing has nothing to allow', () => {
  const markup = renderToStaticMarkup(
    <PluginGrantFields
      value={emptyPluginGrants()}
      requested={[]}
      onChange={() => {}}
    />
  )
  expect(markup).toContain('No host API permissions requested')
  expect(markup).not.toContain('role="checkbox"')
})

test('allowing changes asks what may change and where; without it nothing is asked', () => {
  const without = renderToStaticMarkup(
    <PluginGrantFields
      value={emptyPluginGrants()}
      requested={['api_write']}
      onChange={() => {}}
    />
  )
  expect(without).not.toContain('What it may change')

  const allowed = renderToStaticMarkup(
    <PluginGrantFields
      value={{ ...emptyPluginGrants(), permissions: ['api_write'] }}
      requested={['api_write']}
      onChange={() => {}}
    />
  )
  expect(allowed).toContain('What it may change')
  expect(allowed).toContain('every change this plugin attempts is refused')
  expect(allowed).toContain('Every service')
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
