// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { describe, expect, test } from 'bun:test'
import { PROJECT_SECTION_LINKS } from '@/lib/project-navigation'
import { sectionIcons } from './ProjectSectionLayout'

describe('project section navigation icons', () => {
  test('every link of a rendered section has its own icon', () => {
    // Sections with fewer than two links render no contextual navigation.
    const missing = Object.values(PROJECT_SECTION_LINKS)
      .filter((links) => (links?.length ?? 0) >= 2)
      .flatMap((links) => links ?? [])
      .map((link) => link.url)
      .filter((url) => !(url in sectionIcons))
    expect(missing).toEqual([])
  })
})
