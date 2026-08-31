// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  serviceCategoryIcon,
  toggleServiceTag,
} from './service-template-discovery'

describe('service template discovery', () => {
  it('maps catalog categories to stable icon families', () => {
    expect(serviceCategoryIcon('Developer Tools')).toBe('developer')
    expect(serviceCategoryIcon('Monitoring & Analytics')).toBe('monitoring')
    expect(serviceCategoryIcon('Authentication')).toBe('security')
    expect(serviceCategoryIcon('Email')).toBe('generic')
    expect(serviceCategoryIcon('AI & Machine Learning')).toBe('ai')
    expect(serviceCategoryIcon('Uncategorized')).toBe('generic')
  })

  it('toggles an exact discovery tag', () => {
    expect(toggleServiceTag(null, 'postgres')).toBe('postgres')
    expect(toggleServiceTag('postgres', 'postgres')).toBeNull()
    expect(toggleServiceTag('postgres', 'redis')).toBe('redis')
  })
})
