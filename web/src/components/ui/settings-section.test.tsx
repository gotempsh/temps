// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { Cpu } from 'lucide-react'
import { SettingsSection } from './settings-section'

test('collapsed sections keep form fields mounted for submission', () => {
  const html = renderToStaticMarkup(
    <SettingsSection title="Compute resources" icon={Cpu}>
      <input name="memory" defaultValue="512" />
    </SettingsSection>
  )
  expect(html).toContain('name="memory"')
  expect(html).toContain('value="512"')
  expect(html).not.toContain('open=""')
  expect(html).toContain('<summary')
})
test('primary section can start open without adding a nested form', () => {
  const html = renderToStaticMarkup(
    <SettingsSection title="Identity" icon={Cpu} defaultOpen>
      <input name="name" />
    </SettingsSection>
  )
  expect(html).toContain('open=""')
  expect(html).not.toContain('<form')
})
test('sections with form errors render expanded', () => {
  const html = renderToStaticMarkup(
    <SettingsSection title="Compute" icon={Cpu} hasError>
      <input aria-invalid="true" />
    </SettingsSection>
  )
  expect(html).toContain('open=""')
  expect(html).toContain('aria-invalid="true"')
})
