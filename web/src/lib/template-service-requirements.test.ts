// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  getTemplateServiceRequirements,
  normalizeTemplateServiceType,
} from './template-service-requirements'

const services = [
  { id: 1, name: 'existing-postgres', service_type: 'postgres' },
  { id: 2, name: 'cache', service_type: 'redis' },
]

describe('template service requirements', () => {
  test('normalizes common service aliases', () => {
    expect(normalizeTemplateServiceType(' PostgreSQL ')).toBe('postgres')
    expect(normalizeTemplateServiceType('mysql')).toBe('mariadb')
    expect(normalizeTemplateServiceType('object-storage')).toBe('s3')
  })

  test('exposes matching existing services as one-click choices', () => {
    const [requirement] = getTemplateServiceRequirements(
      ['postgres'],
      services,
      []
    )

    expect(requirement.label).toBe('PostgreSQL')
    expect(requirement.serviceType).toBe('postgres')
    expect(requirement.availableServices.map((service) => service.id)).toEqual([
      1,
    ])
    expect(requirement.isSatisfied).toBeFalse()
  })

  test('marks a requirement satisfied only when a compatible service is selected', () => {
    const [requirement] = getTemplateServiceRequirements(
      ['postgres'],
      services,
      [1, 2]
    )

    expect(requirement.selectedServices.map((service) => service.id)).toEqual([
      1,
    ])
    expect(requirement.isSatisfied).toBeTrue()
  })

  test('deduplicates requirements after normalization', () => {
    const requirements = getTemplateServiceRequirements(
      ['postgres', 'PostgreSQL'],
      services,
      []
    )

    expect(requirements).toHaveLength(1)
  })
})
