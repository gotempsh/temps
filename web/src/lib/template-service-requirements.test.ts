// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  getTemplateServiceRequirements,
  normalizeTemplateServiceType,
  toggleDatabaseSelection,
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
    expect(normalizeTemplateServiceType('rustfs')).toBe('s3')
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

  test('deselects a newly created database instead of forcing it back into submission', () => {
    const result = toggleDatabaseSelection([3], 3, [
      { id: 3, name: 'new-postgres', service_type: 'postgres' },
    ])

    expect(result.selectedServiceIds).toEqual([])
    expect(result.conflictingService).toBeUndefined()
  })

  test('rejects a second provider for the same normalized variable namespace', () => {
    const result = toggleDatabaseSelection([1], 3, [
      ...services,
      { id: 3, name: 'second-postgres', service_type: 'postgresql' },
    ])

    expect(result.selectedServiceIds).toEqual([1])
    expect(result.conflictingService?.name).toBe('existing-postgres')
  })

  test('allows databases with different variable namespaces', () => {
    const result = toggleDatabaseSelection([1], 2, services)

    expect(result.selectedServiceIds).toEqual([1, 2])
    expect(result.conflictingService).toBeUndefined()
  })

  test('treats RustFS as the S3 variable namespace for requirements and collisions', () => {
    const rustfs = { id: 4, name: 'objects', service_type: 'rustfs' }
    const [requirement] = getTemplateServiceRequirements(
      ['s3'],
      [rustfs],
      [rustfs.id]
    )
    const collision = toggleDatabaseSelection([rustfs.id], 5, [
      rustfs,
      { id: 5, name: 'second-objects', service_type: 's3' },
    ])

    expect(requirement.isSatisfied).toBeTrue()
    expect(collision.conflictingService?.id).toBe(rustfs.id)
  })
})
