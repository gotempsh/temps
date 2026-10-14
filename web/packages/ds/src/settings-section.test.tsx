// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { Flag } from 'lucide-react'
import { SettingsSection } from './settings-section'

test('settings sections expose a deep-linkable id and honour defaultOpen', () => {
  const open = renderToStaticMarkup(
    <SettingsSection
      id="settings-section-feature-flags"
      title="Feature flags"
      icon={Flag}
      defaultOpen
    >
      Flags
    </SettingsSection>
  )
  expect(open).toContain('id="settings-section-feature-flags"')
  expect(open).toContain('open=""')

  const closed = renderToStaticMarkup(
    <SettingsSection title="Feature flags" icon={Flag}>
      Flags
    </SettingsSection>
  )
  expect(closed).not.toContain('id=')
  expect(closed).not.toContain('open=""')
})
