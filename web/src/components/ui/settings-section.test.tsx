// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { Cpu } from 'lucide-react'
import { SettingsSection } from './settings-section'

test('sections render their fields open, with no expand control', () => {
  const html = renderToStaticMarkup(
    <SettingsSection title="Compute resources" icon={Cpu}>
      <input name="memory" defaultValue="512" />
    </SettingsSection>
  )
  expect(html).toContain('name="memory"')
  expect(html).toContain('value="512"')
  expect(html).not.toContain('<summary')
  expect(html).not.toContain('<form')
})
test('fields with errors are visible without any interaction', () => {
  const html = renderToStaticMarkup(
    <SettingsSection title="Compute" icon={Cpu} hasError>
      <input aria-invalid="true" />
    </SettingsSection>
  )
  expect(html).toContain('aria-invalid="true"')
  expect(html).not.toContain('<details')
})
