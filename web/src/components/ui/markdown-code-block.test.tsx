// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MarkdownCodeBlock } from './markdown-code-block'

test('fenced code uses shared controls and escapes untrusted markup', () => {
  const html = renderToStaticMarkup(
    <MarkdownCodeBlock>
      <code className="language-html">{'<script>alert(1)</script>\n'}</code>
    </MarkdownCodeBlock>
  )
  expect(html).toContain('shiki-code')
  expect(html).toContain('aria-label="Copy"')
  expect(html).toContain('&lt;script&gt;')
  expect(html).not.toContain('<script>')
})
test('missing language and plain children remain readable', () => {
  expect(
    renderToStaticMarkup(<MarkdownCodeBlock>unknown output</MarkdownCodeBlock>)
  ).toContain('unknown output')
})
