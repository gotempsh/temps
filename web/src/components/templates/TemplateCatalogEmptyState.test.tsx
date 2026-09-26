// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { TemplateCatalogEmptyState } from './TemplateCatalogEmptyState'

test('offers both project creation handoffs when the starter catalog is empty', () => {
  const markup = renderToStaticMarkup(
    <TemplateCatalogEmptyState
      kind="starter"
      onUseGitUrl={() => undefined}
      onBrowseRepositories={() => undefined}
    />
  )

  expect(markup).toContain('No templates available')
  expect(markup).toContain('Use a Git URL')
  expect(markup).toContain('Browse repositories')
})
