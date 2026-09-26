// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { Button } from './button'

test('busy state cannot be overridden by caller aria props', () => {
  const html = renderToStaticMarkup(<Button busy aria-disabled={false} aria-busy={false} busyLabel="Saving…">Save</Button>)
  expect(html).toContain('aria-disabled="true"')
  expect(html).toContain('aria-busy="true"')
  expect(html).toContain('Saving…')
  expect(html).not.toContain(' disabled=')
})
test('busy slotted link retains its child and busy semantics', () => {
  const html = renderToStaticMarkup(<Button asChild busy aria-disabled={false}><a href="/settings">Settings</a></Button>)
  expect(html).toContain('href="/settings"')
  expect(html).toContain('aria-disabled="true"')
})
