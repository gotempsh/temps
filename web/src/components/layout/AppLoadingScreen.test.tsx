// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { AppLoadingScreen, PageSkeleton } from './AppLoadingScreen'

test('the boot screen shows the shell outline, not just a mark', () => {
  const html = renderToStaticMarkup(<AppLoadingScreen />)
  expect(html).toContain('aria-label="Loading Temps"')
  expect(html).toContain('/svg/temps-icon.svg')
  // The sidebar rows and the page body are placeholders too.
  expect(html.match(/animate-pulse/g)?.length ?? 0).toBeGreaterThan(8)
})

test('the page skeleton announces itself as loading', () => {
  const html = renderToStaticMarkup(<PageSkeleton />)
  expect(html).toContain('role="status"')
  expect(html).toContain('aria-label="Loading page"')
})
