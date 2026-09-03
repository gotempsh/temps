// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { PageContainer, PageHeader } from './PageContainer'

describe('PageContainer', () => {
  test('owns responsive page gutters and renders one consistent page title', () => {
    const markup = renderToStaticMarkup(
      <PageContainer width="wide">
        <PageHeader
          title="Backups"
          description="Manage backup storage"
          actions={<button type="button">Add source</button>}
        />
      </PageContainer>
    )

    expect(markup).toContain('px-4 py-6 sm:px-6 lg:px-8')
    expect(markup).toContain('max-w-7xl mx-auto')
    expect(markup).toContain(
      '<h1 class="text-2xl font-semibold tracking-tight">Backups</h1>'
    )
    expect(markup).toContain('sm:flex-row sm:items-start sm:justify-between')
  })
})
