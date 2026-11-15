// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { IconWithStatus } from './IconWithStatus'

const icon = <svg data-icon="sandbox" />

test('the icon always stays, with the state as a dot beside it', () => {
  const html = renderToStaticMarkup(<IconWithStatus icon={icon} tone="bad" />)
  expect(html).toContain('data-icon="sandbox"')
  expect(html).toContain('data-status="bad"')
  expect(html).toContain('bg-destructive')
})

test('nothing to report draws no dot', () => {
  const html = renderToStaticMarkup(<IconWithStatus icon={icon} tone={null} />)
  expect(html).toContain('data-icon="sandbox"')
  expect(html).not.toContain('data-status')
})
