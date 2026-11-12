// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { ErrorBoundary } from './ErrorBoundary'
import { CompactErrorFallback } from './CompactErrorFallback'
import { ErrorFallback } from './ErrorFallback'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { MemoryRouter } from 'react-router'

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

test.each(['default', 'page', 'sidebar', 'header', 'alert'])(
  '%s error recovery offers Discord help without sending error details',
  (variant) => {
    const error = new Error('Private diagnostic information')
    const boundary = new ErrorBoundary({ children: <p>Broken child</p> })
    boundary.state = { hasError: true, error, errorInfo: null }

    const content =
      variant === 'default' ? (
        boundary.render()
      ) : variant === 'page' ? (
        <MemoryRouter>
          <ErrorFallback error={error} resetError={() => {}} />
        </MemoryRouter>
      ) : variant === 'alert' ? (
        <ErrorAlert description={error.message} retry={() => {}} />
      ) : (
        <CompactErrorFallback
          error={error}
          resetError={() => {}}
          minimal={variant === 'header'}
        />
      )

    const container = document.createElement('div')
    container.innerHTML = renderToStaticMarkup(content)
    const help = container.querySelector('a[href*="discord.com"]')
    expect(help?.getAttribute('href')).toBe(
      'https://discord.com/invite/ZunrteG6JX'
    )
    expect(help?.textContent).toContain('Ask on Discord')
    expect(help?.getAttribute('target')).toBe('_blank')
    expect(help?.getAttribute('rel')).toBe('noopener noreferrer')
    expect(container.querySelector('button')?.textContent).toMatch(/try|retry/i)
  }
)
