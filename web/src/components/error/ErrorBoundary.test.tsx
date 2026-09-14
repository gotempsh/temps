// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { ErrorBoundary } from './ErrorBoundary'

test('renders fallback immediately, before componentDidCatch supplies errorInfo', () => {
  const boundary = new ErrorBoundary({
    children: <p>Broken child</p>,
    fallback: () => <p>Contained error</p>,
  })
  boundary.state = {
    hasError: true,
    error: new Error('render failed'),
    errorInfo: null,
  }
  const html = renderToStaticMarkup(boundary.render())
  expect(html).toContain('Contained error')
  expect(html).not.toContain('Broken child')
})
