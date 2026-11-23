// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type { ExternalServiceInfo } from '@/api/client'
import { linkableDatabases } from './database-links'

const db = (id: number, name: string, service_type = 'redis') =>
  ({ id, name, service_type }) as ExternalServiceInfo

test('databases already linked to the service are not offered again', () => {
  const all = [db(1, 'cache'), db(2, 'main', 'postgres'), db(3, 'other')]
  expect(
    linkableDatabases(all, [{ service: all[0] }], '').map((d) => d.id)
  ).toEqual([2, 3])
})

test('the search matches the name or the type, ignoring case and spaces', () => {
  const all = [db(1, 'cache'), db(2, 'main', 'postgres')]
  expect(linkableDatabases(all, [], ' POSTGRES ').map((d) => d.id)).toEqual([2])
  expect(linkableDatabases(all, [], 'cach').map((d) => d.id)).toEqual([1])
  expect(linkableDatabases(all, [], 'nope')).toEqual([])
})
