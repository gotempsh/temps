// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { SandboxWaking } from './ChatPreviewTab'

test('a sandbox being started says so instead of showing a gateway error', () => {
  const markup = renderToStaticMarkup(
    <SandboxWaking error={null} onRetry={() => {}} />
  )
  expect(markup).toContain('Waking this chat')
  expect(markup).toContain('role="status"')
  expect(markup).toContain('animate-pulse')
})

test('a sandbox that could not be started says why and offers another try', () => {
  const markup = renderToStaticMarkup(
    <SandboxWaking error="Docker is not reachable." onRetry={() => {}} />
  )
  expect(markup).toContain('could not be started')
  expect(markup).toContain('Docker is not reachable.')
  expect(markup).toContain('Try again')
  expect(markup).toContain('role="alert"')
})
