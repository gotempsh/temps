// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { Flag } from 'lucide-react'
import { SettingsSection, SettingsSurface } from './settings-section'

const boxes = (html: string) => html.split('rounded-lg border').length - 1

test('settings sections are always open and deep-linkable', () => {
  const html = renderToStaticMarkup(
    <SettingsSection
      id="settings-section-feature-flags"
      title="Feature flags"
      icon={Flag}
    >
      Flags
    </SettingsSection>
  )
  expect(html).toContain('id="settings-section-feature-flags"')
  expect(html).not.toContain('<details')
  expect(html).not.toContain('<summary')
  expect(html).toContain('Flags')
  // An open group, title on the left and settings on the right, not a card.
  expect(boxes(html)).toBe(0)
  expect(html).toContain('md:grid-cols-')
})

test('a section inside another renders flat, never a box in a box', () => {
  const html = renderToStaticMarkup(
    <SettingsSection title="Outer" icon={Flag}>
      <SettingsSection title="Inner" icon={Flag}>
        Inner fields
      </SettingsSection>
    </SettingsSection>
  )
  expect(html).toContain('Inner fields')
  expect(boxes(html)).toBe(0)
})

test('a section inside a page-owned surface renders flat', () => {
  const html = renderToStaticMarkup(
    <SettingsSurface>
      <SettingsSection title="Inner" icon={Flag}>
        Fields
      </SettingsSection>
    </SettingsSurface>
  )
  expect(boxes(html)).toBe(0)
  expect(html).toContain('Fields')
})
