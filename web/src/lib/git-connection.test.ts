// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  authMethodDisplayName,
  parseRepositoryListState,
  providerDisplayName,
} from './git-connection'

test('an empty URL gives the default repository list', () => {
  expect(parseRepositoryListState({})).toEqual({
    page: 1,
    perPage: 20,
    search: '',
    visibility: 'all',
    sort: 'pushed',
  })
})

test('valid URL state is kept as-is', () => {
  expect(
    parseRepositoryListState({
      page: '3',
      per_page: '50',
      q: ' api ',
      visibility: 'private',
      sort: 'name',
    })
  ).toEqual({
    page: 3,
    perPage: 50,
    search: 'api',
    visibility: 'private',
    sort: 'name',
  })
})

test('hand-edited URL values fall back instead of reaching the API', () => {
  expect(
    parseRepositoryListState({
      page: '-2',
      per_page: '5000',
      visibility: 'secret',
      sort: 'toString',
    })
  ).toEqual({
    page: 1,
    perPage: 20,
    search: '',
    visibility: 'all',
    sort: 'pushed',
  })
  expect(parseRepositoryListState({ page: '1.5' }).page).toBe(1)
})

test('provider and auth method names read as the product, not the enum', () => {
  expect(providerDisplayName('github')).toBe('GitHub')
  expect(providerDisplayName('forgejo')).toBe('Forgejo')
  expect(authMethodDisplayName('github_app')).toBe('GitHub App')
  expect(authMethodDisplayName('pat')).toBe('Pat')
})
