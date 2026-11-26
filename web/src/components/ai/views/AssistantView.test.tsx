// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import ReactMarkdown from 'react-markdown'
import { MemoryRouter } from 'react-router'
import { AssistantCodeBlock } from './AssistantCodeBlock'
import { isFigureColumn, type ViewCell } from './view-table'
import { ViewStreamingContext } from './view-streaming'

const add = (path: string, value: unknown) =>
  JSON.stringify({ op: 'add', path, value })
const el = (
  key: string,
  type: string,
  props: unknown,
  children: string[] = []
) => add(`/elements/${key}`, { type, props, children })

/** An answer as the chat draws it: Markdown with the chat's code blocks. */
function answer(markdown: string, streaming = false): string {
  return renderToStaticMarkup(
    <MemoryRouter>
      <ViewStreamingContext.Provider value={streaming}>
        <ReactMarkdown components={{ pre: AssistantCodeBlock }}>
          {markdown}
        </ReactMarkdown>
      </ViewStreamingContext.Provider>
    </MemoryRouter>
  )
}

const view = (...written: string[]) =>
  `Here is what is running.\n\n\`\`\`spec\n${written.join('\n')}\n\`\`\`\n`

const SERVICES = view(
  add('/root', 'main'),
  el('main', 'Stack', {}, ['totals', 'services', 'open']),
  el('totals', 'Stats', {}, ['running']),
  el('running', 'Stat', { label: 'Running', value: '3', tone: 'ok' }),
  el('services', 'Table', {
    columns: ['Service', 'State', 'Docs'],
    rows: [
      [
        { text: 'storefront-web', href: '/projects/storefront-web' },
        { text: 'Running', tone: 'ok' },
        { text: 'Read more', href: 'https://elsewhere.example/docs' },
      ],
    ],
  }),
  el('open', 'Link', { label: 'All projects', href: '/projects' })
)

test('a view block is drawn with the console components, not shown as code', () => {
  const markup = answer(SERVICES)
  expect(markup).toContain('data-assistant-view')
  expect(markup).toContain('Here is what is running.')
  // The shared table, at the density of the console's record lists.
  expect(markup).toContain('[&amp;_th]:h-9')
  expect(markup).toContain('>Service</th>')
  expect(markup).toContain('Running')
  expect(markup).toContain('All projects')
  // The source is not on the page.
  expect(markup).not.toContain('/elements/')
  expect(markup).not.toContain('shiki-code')
})

test('a link stays in the console: any other address is plain text', () => {
  const markup = answer(SERVICES)
  expect(markup).toContain('href="/projects/storefront-web"')
  expect(markup).toContain('href="/projects"')
  expect(markup).toContain('Read more')
  expect(markup).not.toContain('elsewhere.example')
})

test('a button to somewhere else is not drawn', () => {
  const markup = answer(
    view(
      add('/root', 'open'),
      el('open', 'Link', { label: 'Open', href: '//elsewhere.example' })
    )
  )
  expect(markup).not.toContain('elsewhere.example')
  expect(markup).not.toContain('<a ')
})

test('text a model wrote is never markup', () => {
  const markup = answer(
    view(
      add('/root', 'note'),
      el('note', 'Callout', {
        tone: 'warning',
        title: '<img src=x onerror=alert(1)>',
        text: '<script>alert(1)</script>',
      })
    )
  )
  expect(markup).toContain('&lt;script&gt;')
  expect(markup).not.toContain('<script>')
  expect(markup).not.toContain('<img')
})

test('a block that describes no view is shown as written once the answer settles', () => {
  const markup = answer('```spec\nnot a view at all\n```\n')
  expect(markup).not.toContain('data-assistant-view')
  expect(markup).toContain('shiki-code')
  expect(markup).toContain('not a view at all')
})

test('while the answer streams, a view not yet drawable is a placeholder', () => {
  const markup = answer('```spec\n{"op":"add","path":"/ro', true)
  expect(markup).toContain('aria-label="Drawing the view"')
  expect(markup).not.toContain('shiki-code')
})

test('a settled view says when part of it could not be drawn', () => {
  const partial = view(
    add('/root', 'main'),
    el('main', 'Stack', {}, ['state', 'chart']),
    el('state', 'Status', { tone: 'ok', label: 'Running' }),
    el('chart', 'LineChart', { points: [1, 2, 3] })
  )
  const settled = answer(partial)
  expect(settled).toContain('Part of this view could not be drawn.')
  // What the model wrote stays readable, folded under the note.
  expect(settled).toContain('<details')
  expect(settled).toContain('LineChart')
  // Mid-answer the missing part may simply not have arrived.
  expect(answer(partial, true)).not.toContain('could not be drawn')
})

test('every other fenced block is still code', () => {
  const markup = answer('```json\n{"root":"main","elements":{}}\n```\n')
  expect(markup).toContain('shiki-code')
  expect(markup).not.toContain('data-assistant-view')
})

test('a wide table has a stacked form for a narrow chat, so it never scrolls sideways', () => {
  const markup = answer(SERVICES)
  // Below the container width each record is a block of labelled lines.
  expect(markup).toContain('@lg:hidden')
  expect(markup).toContain('hidden @lg:block')
  expect(markup).toContain(
    '<dt class="shrink-0 text-xs text-muted-foreground">State</dt>'
  )
  // Two columns fit anywhere and stay a table.
  const narrow = answer(
    view(
      add('/root', 'table'),
      el('table', 'Table', {
        columns: ['Service', 'State'],
        rows: [['storefront-web', 'Running']],
      })
    )
  )
  expect(narrow).not.toContain('@lg:hidden')
  expect(narrow).toContain('>Service</th>')
})

test('a column of figures is right-aligned in tabular digits; names and dates are not', () => {
  const rows: ViewCell[][] = [
    ['/', '803', '39.2%', '4155 ms', '2026-11-12', '—'],
    ['/cart', '1,829', '+5.0%', '~375 ms', '2026-11-13', '181 (so far)'],
  ]
  expect(
    [0, 1, 2, 3, 4, 5].map((column) => isFigureColumn(rows, column))
  ).toEqual([false, true, true, true, false, true])
  // A column that says nothing is not a column of figures.
  expect(isFigureColumn([['—'], ['']], 0)).toBe(false)

  const markup = answer(
    view(
      add('/root', 'table'),
      el('table', 'Table', {
        columns: ['Page', 'Views'],
        rows: [
          ['/', '803'],
          ['/cart', '117'],
        ],
      })
    )
  )
  expect(markup).toMatch(
    /<td class="[^"]*text-right tabular-nums[^"]*">803<\/td>/
  )
  expect(markup).not.toMatch(/<td class="[^"]*text-right[^"]*">\/<\/td>/)
})
