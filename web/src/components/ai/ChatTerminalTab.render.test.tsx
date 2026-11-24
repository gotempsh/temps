// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { TerminalOnboarding } from './ChatTerminalTab'

test('without a sandbox the tab says what is missing and what it would do', () => {
  const markup = renderToStaticMarkup(
    <TerminalOnboarding reason="No AI sandbox runtime is configured." />
  )
  expect(markup).toContain('No terminal yet')
  expect(markup).toContain('No AI sandbox runtime is configured.')
  expect(markup).toContain('npm test')
})
