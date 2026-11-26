// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { markdownFence } from './markdown-fence'

test('reads the language and text of a fenced block', () => {
  expect(
    markdownFence(<code className="hljs language-spec">{'{"a":1}\n'}</code>)
  ).toEqual({ language: 'spec', code: '{"a":1}' })
})

test('a block with no language, or bare text, has none', () => {
  expect(markdownFence(<code>{['plain ', 'output']}</code>)).toEqual({
    language: undefined,
    code: 'plain output',
  })
  expect(markdownFence('unknown output')).toEqual({
    language: undefined,
    code: 'unknown output',
  })
})
