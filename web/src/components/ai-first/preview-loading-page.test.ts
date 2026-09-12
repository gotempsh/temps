// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { PREVIEW_LOADING_PAGE, showPreviewLoadingPage } from './preview-loading-page'

test('paints an accessible loading page synchronously in the reserved tab', () => {
  const calls: string[] = []
  const tab = { document: {
    open: () => calls.push('open'),
    write: (html: string) => calls.push(html),
    close: () => calls.push('close'),
  } } as unknown as Window
  showPreviewLoadingPage(tab)
  expect(calls).toEqual(['open', PREVIEW_LOADING_PAGE, 'close'])
  expect(PREVIEW_LOADING_PAGE).toContain('<title>Opening preview… · Temps</title>')
  expect(PREVIEW_LOADING_PAGE).toContain('role="status"')
  expect(PREVIEW_LOADING_PAGE).toContain('aria-label="Temps"')
  expect(PREVIEW_LOADING_PAGE).toContain('>temps</text>')
  expect(PREVIEW_LOADING_PAGE).toContain('fill: CanvasText')
  expect(PREVIEW_LOADING_PAGE).toContain('fill: Canvas;')
  expect(PREVIEW_LOADING_PAGE).toContain('prefers-reduced-motion')
  expect(PREVIEW_LOADING_PAGE).not.toContain('<script')
  expect(PREVIEW_LOADING_PAGE).not.toContain('http')
})
