// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { OnboardingChoices } from './OnboardingChoices'

function markup(showHeading = true) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <OnboardingChoices showHeading={showHeading} />
    </MemoryRouter>
  )
}

test('each goal opens its existing setup flow, including template creation and sandbox creation', () => {
  const html = markup()
  const dom = new DOMParser().parseFromString(html, 'text/html')
  expect(
    Array.from(dom.querySelectorAll('nav a')).map((link) => [
      link.textContent?.trim(),
      link.getAttribute('href'),
    ])
  ).toEqual([
    ['Create project', '/projects/new?source=templates'],
    ['Set up email', '/email'],
    ['Create sandbox', '/sandboxes?new=sandbox'],
    ['Connect app', '/projects/new?source=monitor'],
  ])
  expect(html).not.toContain('Deploy sample app')
  expect(html).not.toContain('nginx')
})

test('unconfigured features remain visible with prerequisites and honest effort ranges', () => {
  const html = markup()
  // No query provider or optional configuration is required to discover a path.
  expect(html).toContain('Run a sandbox')
  expect(html).toContain('No AI provider is required')
  expect(html).toContain('verified sender domain')
  expect(html).toContain('SDK or exporter configuration')
  expect(html).toContain('5–10 min')
  expect(html).toContain('3–5 min')
  expect(html).toContain('5–15 min')
  expect(html).toContain('not a completion-time guarantee')
  expect(html).toContain('waiting time')
  expect(html).toContain('href="/tools"')
})

test('the standalone page supplies its own question without a duplicate heading', () => {
  expect(markup()).toContain('What would you like to try first?')
  expect(markup(false)).not.toContain('What would you like to try first?')
})
