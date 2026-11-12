// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { ProtectedLayout } from './ProtectedLayout'

test('server connection failure links to the public documentation', () => {
  const html = renderToStaticMarkup(
    <AuthContext.Provider
      value={{
        user: null,
        isLoading: false,
        error: new TypeError('Failed to fetch'),
        logout: async () => {},
        refetch: () => {},
      }}
    >
      <ProtectedLayout>
        <p>Console</p>
      </ProtectedLayout>
    </AuthContext.Provider>
  )
  expect(html).toContain('Cannot reach the server')
  expect(html).toContain('href="https://temps.sh/docs"')
  expect(html).not.toContain('href="https://docs.temps.sh"')
  const container = document.createElement('div')
  container.innerHTML = html
  const help = container.querySelector('a[href*="discord.com"]')
  expect(help?.getAttribute('href')).toBe(
    'https://discord.com/invite/ZunrteG6JX'
  )
  expect(help?.textContent).toContain('Ask on Discord')
  expect(help?.getAttribute('target')).toBe('_blank')
  expect(help?.getAttribute('rel')).toBe('noopener noreferrer')
  expect(html).toContain('href="https://github.com/gotempsh/temps/issues"')
})
