// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'

/**
 * A "Type <name> (to confirm)" prompt shows the name through
 * `ConfirmNameBadge`, so the user can copy it instead of retyping it. A plain
 * `<code>`, `<span>` or `<strong>` there is the copy button forgotten.
 */
const PLAIN_NAME_PROMPT =
  /(?:^|>)\s*Type(?:\{' '\}|\s)+<(?!ConfirmNameBadge\b)(?:code|span|strong|kbd|b|em)\b[^>]*>/gm

export function plainNamePrompts(source: string): string[] {
  return Array.from(source.matchAll(PLAIN_NAME_PROMPT), (m) => m[0].trim())
}

const root = new URL('../../../', import.meta.url).pathname

test('the scan catches a name rendered without the copy button', () => {
  expect(
    plainNamePrompts('<Label>\n  Type <code>{service.name}</code> to confirm')
  ).toHaveLength(1)
  expect(
    plainNamePrompts(
      '<span>\n  Type{\' \'}\n  <span className="font-mono">\n    {PHRASE}\n  </span>\n</span>'
    )
  ).toHaveLength(1)
  expect(
    plainNamePrompts(
      'Type <ConfirmNameBadge value={service.name} /> to confirm'
    )
  ).toEqual([])
})

test('every "Type <name> to confirm" prompt offers a copy button', async () => {
  const glob = new Bun.Glob('src/**/*.tsx')
  const offenders: string[] = []
  for await (const file of glob.scan({ cwd: root })) {
    if (file.includes('.test.')) continue
    const source = await Bun.file(`${root}${file}`).text()
    for (const prompt of plainNamePrompts(source))
      offenders.push(`${file}: ${prompt.replace(/\s+/g, ' ')}`)
  }
  expect(offenders).toEqual([])
})
