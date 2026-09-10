// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { completeServiceCreation } from './service-creation-success'

describe('completeServiceCreation', () => {
  test('emits one default toast and completes the parent callback once', () => {
    const notifications: string[] = []
    const completedServiceIds: number[] = []

    completeServiceCreation({
      createdService: { id: 42, name: 'postgres-ymtu' },
      notifySuccess: (message) => notifications.push(message),
      onSuccess: (service) => completedServiceIds.push(service.id),
    })

    expect(notifications).toEqual(['Service created successfully'])
    expect(completedServiceIds).toEqual([42])
  })

  test('uses the embedding workflow message without adding a generic toast', () => {
    const notifications: string[] = []

    completeServiceCreation({
      createdService: { id: 42, name: 'postgres-ymtu' },
      notifySuccess: (message) => notifications.push(message),
      onSuccess: () => {},
      successMessage: (service) =>
        `Database "${service.name}" created successfully!`,
    })

    expect(notifications).toEqual([
      'Database "postgres-ymtu" created successfully!',
    ])
  })
})
