// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { PROJECT_GRID_CLASS } from './project-grid'

test('the project list keeps one bounded column on a phone', () => {
  expect(PROJECT_GRID_CLASS.split(' ')).toContain('grid-cols-1')
})
