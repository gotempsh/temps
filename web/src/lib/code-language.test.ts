// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { codeLanguage, codeLanguageForFile } from './code-language'

test('fence aliases resolve to bundled grammars', () => {
  for (const [hint, expected] of [
    ['language-ts', 'typescript'],
    ['TSX', 'tsx'],
    ['jsx', 'tsx'],
    ['sh', 'shell'],
    ['yml', 'yaml'],
    ['rust', 'rust'],
    ['sql', 'sql'],
  ] as const) {
    expect(codeLanguage(hint)).toBe(expected)
  }
  expect(codeLanguage('unsupported')).toBe('text')
  expect(codeLanguage()).toBe('text')
})
test('source previews resolve file names and strip URL suffixes', () => {
  expect(codeLanguageForFile('/app/main.py?version=1')).toBe('python')
  expect(codeLanguageForFile('Dockerfile.prod')).toBe('dockerfile')
  expect(codeLanguageForFile('.env.local')).toBe('bash')
  expect(codeLanguageForFile('app.tsx')).toBe('tsx')
  expect(codeLanguageForFile('unknown.bin')).toBe('text')
})
