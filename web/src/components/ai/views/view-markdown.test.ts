// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { viewsAsMarkdown } from './view-markdown'

const add = (path: string, value: unknown) =>
  JSON.stringify({ op: 'add', path, value })
const el = (
  key: string,
  type: string,
  props: unknown,
  children: string[] = []
) => add(`/elements/${key}`, { type, props, children })
const fenced = (...written: string[]) =>
  '```spec\n' + written.join('\n') + '\n```'

test('a copied answer carries what its view showed, as Markdown', () => {
  const answer = [
    'Here is the week.',
    '',
    fenced(
      add('/root', 'main'),
      el('main', 'Stack', {}, [
        'totals',
        'pages',
        'daily',
        'channels',
        'note',
        'open',
      ]),
      el('totals', 'Stats', {}, ['visitors', 'bounce']),
      el('visitors', 'Stat', { label: 'Visitors', value: '568' }),
      el('bounce', 'Stat', {
        label: 'Bounce rate',
        value: '39.2%',
        detail: 'avg 75.6s',
      }),
      el('pages', 'Table', { columns: ['Page', 'Views'], rows: [] }),
      add('/elements/pages/props/rows/-', [
        { text: 'web', href: '/projects/web' },
        803,
      ]),
      add('/elements/pages/props/rows/-', ['a | b', '337']),
      el('daily', 'Trend', {
        label: 'Visitors per day',
        partial: true,
        points: [
          { at: 'Nov 24', value: 118 },
          { at: 'Nov 25', value: 157 },
        ],
      }),
      el('channels', 'Bars', {
        items: [
          { label: 'Direct', value: 373, text: '373 (30.2%)' },
          { label: 'Email', value: 83 },
        ],
      }),
      el('note', 'Callout', {
        tone: 'info',
        title: 'Partial day',
        text: 'Today is still in progress.',
      }),
      el('open', 'Link', { label: 'Open project', href: '/projects/web' })
    ),
    '',
    'Traffic is trending up.',
  ].join('\n')

  expect(viewsAsMarkdown(answer)).toBe(
    [
      'Here is the week.',
      '',
      '- Visitors: **568**',
      '- Bounce rate: **39.2%** (avg 75.6s)',
      '',
      '| Page | Views |',
      '| --- | --- |',
      '| [web](/projects/web) | 803 |',
      '| a \\| b | 337 |',
      '',
      '|  | Visitors per day |',
      '| --- | --- |',
      '| Nov 24 | 118 |',
      '| Nov 25 | 157 (so far) |',
      '',
      '- Direct: 373 (30.2%)',
      '- Email: 83',
      '',
      '> **Partial day** Today is still in progress.',
      '',
      '[Open project](/projects/web)',
      '',
      'Traffic is trending up.',
    ].join('\n')
  )
})

test('text with no view, and a block that is no view, are left as written', () => {
  expect(viewsAsMarkdown('Nothing to draw.')).toBe('Nothing to draw.')
  const broken = 'Before\n\n```spec\nnot a view\n```\n\nAfter'
  expect(viewsAsMarkdown(broken)).toBe(broken)
  const code = '```json\n{"root":"main"}\n```'
  expect(viewsAsMarkdown(code)).toBe(code)
})

test('an address outside the console is copied as plain text', () => {
  const copied = viewsAsMarkdown(
    fenced(
      add('/root', 'facts'),
      el('facts', 'KeyValues', {
        items: [
          { label: 'Project', value: 'web', href: '/projects/web' },
          {
            label: 'Docs',
            value: 'Read more',
            href: 'https://elsewhere.example',
          },
        ],
      })
    )
  )
  expect(copied).toBe('- Project: [web](/projects/web)\n- Docs: Read more')
})
