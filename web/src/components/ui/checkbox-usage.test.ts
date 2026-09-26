// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'

test('console controls use the shared Checkbox rather than native inputs', async () => {
  const root = new URL('../../', import.meta.url).pathname
  const violations: string[] = []
  for await (const path of new Bun.Glob('**/*.tsx').scan(root)) {
    const source = await Bun.file(`${root}/${path}`).text()
    if (/<(?:input|Input)\b[^>]*\btype\s*=\s*["']checkbox["']/s.test(source)) {
      violations.push(path)
    }
  }
  expect(violations).toEqual([])
})
