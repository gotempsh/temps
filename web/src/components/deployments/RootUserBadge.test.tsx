// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { describeImageUser, NON_ROOT_USER_DOCS_URL } from './root-user'
import { RootUserBadge } from './RootUserBadge'

installBrowserWindow()
afterEach(cleanup)

test('a deployment that does not run as root shows nothing', () => {
  for (const runsAsRoot of [false, undefined, null]) {
    const { container } = render(
      <RootUserBadge runsAsRoot={runsAsRoot} imageUser="nodejs" />
    )
    expect(container.innerHTML).toBe('')
    cleanup()
  }
})

test('a root deployment shows a labelled badge that opens the fix and docs', async () => {
  const view = render(<RootUserBadge runsAsRoot imageUser={null} />)
  const trigger = view.getByRole('button', {
    name: 'Runs as root: how to run as a non-root user',
  })
  expect(trigger.textContent).toContain('Runs as root')

  fireEvent.click(trigger)
  await waitFor(() =>
    expect(view.getByText('This image runs as root (uid 0)')).toBeTruthy()
  )
  const snippet = view.getByText(/RUN useradd -r -u 10001 app/).textContent
  expect(snippet).toContain('USER app')
  // Alpine has no `useradd`: its command sits beside the Debian one.
  expect(snippet).toContain('# Debian/Ubuntu')
  expect(snippet).toContain('RUN adduser -S -u 10001 app')
  const docs = view.getByRole('link', { name: 'How to run as a non-root user' })
  expect(docs.getAttribute('href')).toBe(NON_ROOT_USER_DOCS_URL)
  expect(
    view.getByRole('button', { name: 'Copy the non-root USER lines' })
  ).toBeTruthy()
})

test('the explanation names what the image declares', () => {
  expect(describeImageUser(null)).toContain('sets no USER')
  expect(describeImageUser('0:0')).toBe(
    'Its Dockerfile sets USER 0:0, which is uid 0.'
  )
})
