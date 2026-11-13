// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import { expect, test } from 'bun:test'

function testFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) return testFiles(path)
    return /\.test\.tsx?$/.test(name) ? [path] : []
  })
}

// `mock.module` cannot be undone: the fake stays in place for every test file
// that runs after this one in the same `bun test` process. A fake auth context
// or data hook there makes unrelated files fail depending on file order.
test('no test replaces a shared context, hook or API module with mock.module', () => {
  const src = join(import.meta.dir, '..')
  const shared = /mock\.module\(\s*['"]@\/(contexts|hooks|api)\//
  const offenders = testFiles(src).filter(
    (path) =>
      path !== import.meta.path && shared.test(readFileSync(path, 'utf8'))
  )
  expect(offenders.map((path) => relative(src, path))).toEqual([])
})
