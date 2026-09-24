// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { getRepositoryUrl } from './repository-url'

test('clone URLs normalize to a browsable https URL', () => {
  expect(
    getRepositoryUrl({ clone_url: 'https://git.example.com/acme/api.git' })
  ).toBe('https://git.example.com/acme/api')
  expect(
    getRepositoryUrl({ ssh_url: 'git@git.example.com:acme/api.git' })
  ).toBe('https://git.example.com/acme/api')
})

test('prefers the HTTPS clone URL and rejects anything unbrowsable', () => {
  expect(
    getRepositoryUrl({
      clone_url: 'https://git.example.com/acme/web',
      ssh_url: 'git@other.example.com:acme/web.git',
    })
  ).toBe('https://git.example.com/acme/web')
  expect(getRepositoryUrl({ clone_url: 'file:///srv/repo' })).toBeNull()
  expect(getRepositoryUrl({})).toBeNull()
})
