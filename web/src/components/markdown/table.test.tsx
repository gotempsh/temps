// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { markdownTable } from './table'

const render = (markdown: string) =>
  renderToStaticMarkup(
    <div className="prose">
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={markdownTable}>
        {markdown}
      </ReactMarkdown>
    </div>
  )

const LIST = `
| id | Project | Deployed |
| --- | --- | ---: |
| 1 | [storefront-web](/projects/storefront-web) | 2026-11-24 |
| 2 | storefront-api | never |
`

test('a Markdown table is the console table, in its bordered surface', () => {
  const markup = render(LIST)
  // The surface, kept out of the typography plugin's table rules.
  expect(markup).toContain('not-prose my-3 overflow-hidden rounded-md border')
  // The shared table at the density of the console's record lists.
  expect(markup).toContain('[&amp;_td]:px-3')
  expect(markup).toContain('[&amp;_th]:h-9')
  // Header cells and rows carry the shared styles, not bare elements.
  expect(markup).toContain('font-medium text-muted-foreground')
  expect(markup).toContain('border-b transition-colors hover:bg-muted/50')
  expect(markup).not.toContain('<table>')
  expect(markup).not.toContain('<td>')
})

test('it scrolls inside its own surface instead of widening the chat', () => {
  expect(render(LIST)).toContain('relative w-full overflow-auto')
})

test('the content is kept: every cell, links and column alignment', () => {
  const markup = render(LIST)
  expect(markup.match(/<th /g)).toHaveLength(3)
  expect(markup.match(/<td /g)).toHaveLength(6)
  expect(markup).toContain('href="/projects/storefront-web"')
  expect(markup).toContain('text-align:right')
})

test('text without a table is left alone', () => {
  const markup = render('Eight projects, none failing.')
  expect(markup).not.toContain('rounded-md border')
  expect(markup).toContain('<p>Eight projects, none failing.</p>')
})
