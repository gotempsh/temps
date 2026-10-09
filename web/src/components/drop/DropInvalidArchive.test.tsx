// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { DropInvalidArchive } from './DropInvalidArchive'

test('explains archive replacement and preserves expandable diagnostics', () => {
  const html = renderToStaticMarkup(
    <DropInvalidArchive details="ZIP end-of-central-directory record not found" />
  )
  expect(html).toContain('role="alert"')
  expect(html).toContain('Invalid ZIP archive')
  expect(html).toContain('not a valid or supported ZIP archive')
  expect(html).toContain('Create a new ZIP from your project folder')
  expect(html).toContain('Renaming another file to .zip')
  expect(html).toContain('<details>')
  expect(html).toContain('Technical details')
  expect(html).toContain('ZIP end-of-central-directory record not found')
})

test('shows the entry correction before expandable technical details', () => {
  const reason =
    'Symbolic links are not supported. Replace links with actual files.'
  const html = renderToStaticMarkup(
    <DropInvalidArchive
      details="Symbolic link entry is not allowed"
      reason={reason}
    />
  )
  expect(html.indexOf(reason)).toBeLessThan(html.indexOf('<details>'))
})
