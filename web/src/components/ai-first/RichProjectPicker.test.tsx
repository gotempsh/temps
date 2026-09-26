// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { ProjectIdentity } from './RichProjectPicker'
import { projectFaviconUrl } from './rich-project-picker'

describe('ProjectIdentity', () => {
  test('renders the project favicon, slug, and deployment status', () => {
    const html = renderToStaticMarkup(
      <ProjectIdentity
        project={{
          id: 42,
          name: 'Checkout API',
          slug: 'checkout-api',
          status: 'ready',
          tone: 'healthy',
        }}
      />
    )

    expect(projectFaviconUrl(42)).toBe('/api/projects/42/favicon')
    expect(html).toContain('Checkout API')
    expect(html).toContain('checkout-api')
    expect(html).toContain('ready')
    expect(html).toContain('bg-emerald-500')
  })
})
