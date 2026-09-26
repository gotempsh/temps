// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { GitProviderMark } from './git-provider-mark'

test.each([
  ['GitHub', 'GitHub'],
  ['github_app', 'GitHub'],
  ['gitlab', 'GitLab'],
  ['Bitbucket', 'Bitbucket'],
  ['gitea', 'Gitea'],
])(
  'reuses the existing %s mark without duplicating announcements',
  (provider, title) => {
    const markup = renderToStaticMarkup(<GitProviderMark provider={provider} />)
    expect(markup).toContain(`<title>${title}</title>`)
    expect(markup).toContain('aria-hidden="true"')
    expect(markup).not.toContain('[&amp;_path]:fill-current')
  }
)

test('supports a named standalone mark', () => {
  const markup = renderToStaticMarkup(
    <GitProviderMark provider="github" label="Source provider: GitHub" />
  )
  expect(markup).toContain('role="img" aria-label="Source provider: GitHub"')
})

test('unknown providers use an unfilled branch icon', () => {
  const markup = renderToStaticMarkup(<GitProviderMark provider="unknown" />)
  expect(markup).toContain('lucide-git-branch')
  expect(markup).not.toContain('[&amp;_path]:fill-current')
})

test('monochrome marks follow the surrounding text color', () => {
  const markup = renderToStaticMarkup(
    <GitProviderMark provider="gitlab" variant="monochrome" />
  )
  expect(markup).toContain('[&amp;_path]:fill-current')
})
