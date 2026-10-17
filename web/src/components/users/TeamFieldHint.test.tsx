// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { TeamFieldHint } from './TeamFieldHint'

function render(status: Parameters<typeof TeamFieldHint>[0]['status']) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <TeamFieldHint status={status} />
    </MemoryRouter>
  )
}

test('no teams explains why the list is empty and links to Teams', () => {
  const html = render('empty')
  expect(html).toContain('No teams exist yet')
  expect(html).toContain('href="/settings/teams"')
})

test('a failed teams read says so and never claims there are no teams', () => {
  const html = render('error')
  expect(html).toContain('could not be loaded')
  expect(html).not.toContain('No teams exist yet')
})

test('a pending or populated list keeps the plain optional note', () => {
  for (const status of ['pending', 'available'] as const) {
    const html = render(status)
    expect(html).toContain('Adds the user to the selected team')
    expect(html).not.toContain('No teams exist yet')
  }
})
