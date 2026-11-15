// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { LocalScreenshotNote } from './LocalScreenshotNote'

test('the local screenshot note says what the browser may reach and the sandbox fallback', () => {
  const html = renderToStaticMarkup(<LocalScreenshotNote />)
  expect(html).toContain('headless Chromium on this server')
  expect(html).toContain('private, loopback and link-local addresses')
  expect(html).toContain('without the sandbox')
})
