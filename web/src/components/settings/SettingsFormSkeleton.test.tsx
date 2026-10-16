// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { SettingsFormSkeleton } from './SettingsFormSkeleton'

test('keeps the page heading visible and names the loading region', () => {
  const html = renderToStaticMarkup(
    <SettingsFormSkeleton title="Build limits" label="Loading build limits" />
  )
  expect(html).toContain('Build limits')
  expect(html).toContain('aria-label="Loading build limits"')
  expect(html).toContain('aria-busy="true"')
})

test('renders placeholders, never a spinner', () => {
  const html = renderToStaticMarkup(
    <SettingsFormSkeleton label="Loading security settings" />
  )
  expect(html).toContain('animate-pulse')
  expect(html).not.toContain('animate-spin')
  expect(html).not.toContain('<h1')
})
