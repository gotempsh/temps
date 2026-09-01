// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { isPlatformToolsRoute } from './platform-navigation'

describe('platform navigation route resolution', () => {
  test('groups secondary platform capabilities under all platform tools', () => {
    expect(isPlatformToolsRoute('/proxy-logs/31')).toBe(true)
    expect(isPlatformToolsRoute('/dns-providers/add')).toBe(true)
  })

  test('keeps primary destinations and settings outside the tools group', () => {
    expect(isPlatformToolsRoute('/projects')).toBe(false)
    expect(isPlatformToolsRoute('/storage')).toBe(false)
    expect(isPlatformToolsRoute('/domains')).toBe(false)
    expect(isPlatformToolsRoute('/git-providers/add')).toBe(false)
    expect(isPlatformToolsRoute('/backups/schedules/2')).toBe(false)
    expect(isPlatformToolsRoute('/proxy')).toBe(false)
    expect(isPlatformToolsRoute('/monitoring')).toBe(false)
    expect(isPlatformToolsRoute('/settings')).toBe(false)
  })

  test('does not accept a similar unrelated prefix', () => {
    expect(isPlatformToolsRoute('/toolsmith')).toBe(false)
    expect(isPlatformToolsRoute('/proxying')).toBe(false)
  })
})
