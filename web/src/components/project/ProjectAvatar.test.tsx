// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { ProjectAvatar } from './ProjectAvatar'

describe('ProjectAvatar', () => {
  test('renders a deterministic fallback without requesting a missing favicon', () => {
    const markup = renderToStaticMarkup(<ProjectAvatar name="Example" />)

    expect(markup).toContain('E')
    expect(markup).not.toContain('<img')
    expect(markup).not.toContain('/favicon')
  })
})
