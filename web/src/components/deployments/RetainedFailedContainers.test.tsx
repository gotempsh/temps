// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { RetainedFailedContainersLoadError } from './RetainedFailedContainers'

describe('RetainedFailedContainersLoadError', () => {
  test('does not hide a retained-container lookup failure', () => {
    const markup = renderToStaticMarkup(
      <RetainedFailedContainersLoadError onRetry={() => undefined} />
    )

    expect(markup).toContain('Could not load retained deployment containers')
    expect(markup).toContain('Retry')
  })
})
