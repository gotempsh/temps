// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  composeSourceDraftForProject,
  composeSourceExpectedRevision,
  updateComposeSourceDraft,
} from './compose-source-draft'

describe('editable Compose draft concurrency', () => {
  test('keeps the revision where editing began after a query refetch', () => {
    const draft = updateComposeSourceDraft(
      null,
      7,
      'services:\n  app:\n    image: app:2\n',
      12
    )

    expect(composeSourceExpectedRevision(draft, 7, 13)).toBe(12)
  })

  test('preserves a null base revision while initializing a source', () => {
    const draft = updateComposeSourceDraft(null, 7, 'services: {}\n', null)

    expect(composeSourceExpectedRevision(draft, 7, 13)).toBeNull()
  })

  test('does not expose one projects draft to another project', () => {
    const firstProjectDraft = updateComposeSourceDraft(
      null,
      7,
      'services:\n  first:\n    image: first:1\n',
      12
    )

    expect(composeSourceDraftForProject(firstProjectDraft, 8)).toBeNull()
    const secondProjectDraft = updateComposeSourceDraft(
      firstProjectDraft,
      8,
      'services:\n  second:\n    image: second:1\n',
      22
    )
    expect(secondProjectDraft.baseRevision).toBe(22)
  })
})
