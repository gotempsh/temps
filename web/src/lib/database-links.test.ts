// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type { ExternalServiceInfo } from '@/api/client'
import { appsThatCanLink, linkableDatabases } from './database-links'

const db = (id: number, name: string, service_type = 'redis') =>
  ({ id, name, service_type }) as ExternalServiceInfo

test('databases already linked to the service are not offered again', () => {
  const all = [db(1, 'cache'), db(2, 'main', 'postgres')]
  expect(
    linkableDatabases(all, [{ service: all[0] }], '').map((d) => d.id)
  ).toEqual([2])
})

test('a type the service already uses is not offered: the server refuses a second', () => {
  const all = [
    db(1, 'cache'),
    db(2, 'main', 'postgres'),
    db(3, 'sessions'),
    db(4, 'archive', 'postgres'),
  ]
  expect(
    linkableDatabases(all, [{ service: all[0] }], '').map((d) => d.id)
  ).toEqual([2, 4])
})

test('the search matches the name or the type, ignoring case and spaces', () => {
  const all = [db(1, 'cache'), db(2, 'main', 'postgres')]
  expect(linkableDatabases(all, [], ' POSTGRES ').map((d) => d.id)).toEqual([2])
  expect(linkableDatabases(all, [], 'cach').map((d) => d.id)).toEqual([1])
  expect(linkableDatabases(all, [], 'nope')).toEqual([])
})

test('a database is offered to the apps using no database of its type', () => {
  const web = { id: 1 }
  const worker = { id: 2 }
  const admin = { id: 3 }
  const main = { service: db(10, 'main', 'postgres'), usedBy: [web] }
  const archive = { service: db(11, 'archive', 'postgres'), usedBy: [worker] }
  const cache = { service: db(12, 'cache'), usedBy: [web] }
  const databases = [main, archive, cache]
  const apps = [web, worker, admin]
  // web uses it, worker uses another Postgres: only admin can take it.
  expect(appsThatCanLink(main, apps, databases)).toEqual([admin])
  // No one but web uses Redis.
  expect(appsThatCanLink(cache, apps, databases)).toEqual([worker, admin])
})
