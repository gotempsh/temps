// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import {
  CAPABILITIES_DOCS_URL,
  DISCORD_INVITE_URL,
  SIDEBAR_HELP_LINKS,
} from './support'

describe('SIDEBAR_HELP_LINKS', () => {
  test('lists what Temps supports first, then Discord', () => {
    expect(SIDEBAR_HELP_LINKS.map((link) => [link.label, link.href])).toEqual([
      ['What Temps supports', 'https://temps.sh/docs/capabilities'],
      ['Need help? Join Discord', DISCORD_INVITE_URL],
    ])
  })

  test('gives every link a unique id and an icon', () => {
    const ids = SIDEBAR_HELP_LINKS.map((link) => link.id)
    expect(new Set(ids).size).toBe(ids.length)
    for (const link of SIDEBAR_HELP_LINKS) {
      expect(link.icon).toBeDefined()
    }
  })

  test('points at the same page the install script prints', () => {
    const installer = readFileSync(
      join(import.meta.dir, '../../../scripts/install.sh'),
      'utf8'
    )
    expect(installer).toContain(`capabilities_url=${CAPABILITIES_DOCS_URL}`)
  })
})
