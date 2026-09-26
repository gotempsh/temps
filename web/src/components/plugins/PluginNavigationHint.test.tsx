// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { PluginNavigationHint } from './PluginNavigationHint'
import type { NavEntry, NavSection } from '../../types/plugins'

const entry = (section: NavSection): NavEntry => ({
  section,
  label: 'Example',
  icon: 'puzzle',
  path: '/',
  order: 0,
})

test('explains a running plugin without navigation rather than implying install failed', () => {
  expect(renderToStaticMarkup(<PluginNavigationHint nav={[]} />)).toContain(
    'declares no sidebar page'
  )
})
test('points to contextual sidebars and does not warn for platform navigation', () => {
  expect(
    renderToStaticMarkup(<PluginNavigationHint nav={[entry('project')]} />)
  ).toContain('Open a project')
  expect(
    renderToStaticMarkup(<PluginNavigationHint nav={[entry('settings')]} />)
  ).toContain('Settings sidebar')
  expect(
    renderToStaticMarkup(<PluginNavigationHint nav={[entry('platform')]} />)
  ).toBe('')
})
