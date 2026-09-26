// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { AnsiLogMessage } from './AnsiLogMessage'

test('renders terminal colors and emphasis without displaying escape codes', () => {
  const html = renderToStaticMarkup(
    <AnsiLogMessage
      message={'\u001b[32mINFO\u001b[0m \u001b[1mready\u001b[0m'}
    />
  )
  expect(html).toContain('color:#0A0')
  expect(html).toContain('<b>ready</b>')
  expect(html).not.toContain('\u001b')
})

test('escapes untrusted HTML and preserves ASCII layout', () => {
  const html = renderToStaticMarkup(
    <AnsiLogMessage
      message={'<script>alert(1)</script>\n  +-- worker & queue'}
    />
  )
  expect(html).not.toContain('<script>')
  expect(html).toContain('&lt;script&gt;')
  expect(html).toContain('\n  +-- worker &amp; queue')
})

test('does not leak color state between log messages', () => {
  renderToStaticMarkup(<AnsiLogMessage message={'\u001b[31merror'} />)
  expect(renderToStaticMarkup(<AnsiLogMessage message="plain" />)).toBe(
    '<span>plain</span>'
  )
})
