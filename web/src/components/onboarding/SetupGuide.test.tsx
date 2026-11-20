// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { SetupGuide, SnippetTabs } from './SetupGuide'

test('steps are numbered, and a done step is marked for screen readers too', () => {
  const html = renderToStaticMarkup(
    <SetupGuide
      title="Get started"
      description="Nothing here yet."
      steps={[
        { title: 'First', caption: 'Do this', done: true },
        { title: 'Second', caption: 'Then this', content: <p>code</p> },
      ]}
    />
  )
  expect(html).toContain('Get started')
  expect(html).toContain('Nothing here yet.')
  expect(html).toContain('First<span class="sr-only"> (done)</span>')
  expect(html).toContain('>2</span>')
  expect(html).not.toContain('>1</span>')
  expect(html).toContain('<p>code</p>')
})

test('snippet tabs open the first stack with its install command', () => {
  const html = renderToStaticMarkup(
    <SnippetTabs
      snippets={[
        {
          value: 'node',
          label: 'Node.js',
          install: 'npm install example',
          code: 'run()',
          language: 'typescript',
        },
        { value: 'python', label: 'Python', code: 'run()', language: 'python' },
      ]}
    />
  )
  expect(html).toContain('Node.js')
  expect(html).toContain('Python')
  expect(html).toContain('npm install example')
})
