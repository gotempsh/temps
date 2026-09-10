// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { consumeDropFilesHandoff, handOffDropFiles } from './drop-handoff'

describe('drop file handoff', () => {
  test('transfers files exactly once without putting them in browser history', () => {
    const file = new File(['hello'], 'index.html', { type: 'text/html' })
    handOffDropFiles([{ file, path: 'site/index.html' }])

    expect(consumeDropFilesHandoff()).toEqual([
      { file, path: 'site/index.html' },
    ])
    expect(consumeDropFilesHandoff()).toBeNull()
  })
})
