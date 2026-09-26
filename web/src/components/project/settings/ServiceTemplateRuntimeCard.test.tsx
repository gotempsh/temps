// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, test } from 'bun:test'
import { ServiceTemplateCatalogUnavailable } from './ServiceTemplateRuntimeCard'

describe('ServiceTemplateCatalogUnavailable', () => {
  test('keeps the applied runtime actionable when catalog upgrades are unavailable', () => {
    const markup = renderToStaticMarkup(
      <ServiceTemplateCatalogUnavailable message="The active catalog could not be read." />
    )

    expect(markup).toContain('Template updates unavailable')
    expect(markup).toContain('The active catalog could not be read.')
    expect(markup).toContain('saved runtime remains deployable and editable')
  })
})
