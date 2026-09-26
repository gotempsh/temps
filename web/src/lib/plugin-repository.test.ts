// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import {
  repositoryInstallSchema,
  repositoryInstallBody,
  repositorySelectionValues,
} from './plugin-repository'

const valid = {
  name: 'my-plugin',
  repository_url: 'https://github.com/example/plugin',
  ref_name: 'v1.0.0',
  trusted: true,
}
test('accepts a GitHub plugin with explicit trust', () =>
  expect(repositoryInstallSchema.safeParse(valid).success).toBe(true))
test.each([
  'https://token@github.com/example/plugin',
  'https://github.com/example/plugin?token=x',
  'https://example.com/example/plugin',
  'https://github.com/example/../plugin',
])('rejects unsafe repository %s', (repository_url) => {
  expect(
    repositoryInstallSchema.safeParse({ ...valid, repository_url }).success
  ).toBe(false)
})
test.each(['--upload-pack=evil', '../main', 'main;env'])(
  'rejects unsafe Git ref %s',
  (ref_name) => {
    expect(
      repositoryInstallSchema.safeParse({ ...valid, ref_name }).success
    ).toBe(false)
  }
)
test('accepts repository-only installation with explicit trust', () => {
  const values = repositoryInstallSchema.parse({
    repository_url: valid.repository_url,
    trusted: true,
  })
  expect(repositoryInstallBody(values)).toEqual({
    repository_url: valid.repository_url,
  })
})
test('omits blank advanced fields and never sends the UI trust field', () => {
  const values = repositoryInstallSchema.parse({
    ...valid,
    name: '',
    ref_name: '',
  })
  expect(repositoryInstallBody(values)).toEqual({
    repository_url: valid.repository_url,
  })
})
test('preserves explicit advanced overrides', () => {
  expect(repositoryInstallBody(valid)).toEqual({
    name: valid.name,
    repository_url: valid.repository_url,
    ref_name: valid.ref_name,
  })
})
test('requires explicit trust before sending install', () =>
  expect(
    repositoryInstallSchema.safeParse({ ...valid, trusted: false }).success
  ).toBe(false))

test('catalog selection pins the exact revision and requires fresh trust', () => {
  const selected = repositorySelectionValues({
    name: 'demo',
    repository: 'https://github.com/example/demo',
    commit: 'a'.repeat(40),
  })
  expect(selected.trusted).toBe(false)
  expect(repositoryInstallSchema.safeParse(selected).success).toBe(false)
  expect(repositoryInstallBody({ ...selected, trusted: true })).toEqual({
    name: 'demo',
    repository_url: 'https://github.com/example/demo',
    ref_name: 'a'.repeat(40),
  })
  expect(repositorySelectionValues(null)).toEqual({
    name: '',
    repository_url: '',
    ref_name: '',
    trusted: false,
  })
})

test('catalog selection retains the path and pins the reviewed commit instead of the moving ref', () => {
  const values = repositorySelectionValues({
    name: 'demo',
    repository: valid.repository_url,
    commit: 'a'.repeat(40),
    path: 'plugins/demo',
    ref: 'release/v2',
  })
  expect(repositoryInstallBody(values)).toEqual({
    name: 'demo',
    repository_url: valid.repository_url,
    ref_name: 'a'.repeat(40),
    path: 'plugins/demo',
  })
  expect(repositorySelectionValues(null).path).toBeUndefined()
})
for (const path of [
  '../x',
  '/x',
  'a//b',
  'a/./b',
  'a/../b',
  'a/',
  'a\\b',
  '.git/x',
  'a/.GIT/x',
  '%2froot',
  'a'.repeat(513),
]) {
  test(`rejects unsafe plugin path ${path}`, () =>
    expect(repositoryInstallSchema.safeParse({ ...valid, path }).success).toBe(
      false
    ))
}
test('supports plugin directory and slash-containing tag', () =>
  expect(
    repositoryInstallSchema.safeParse({
      ...valid,
      path: 'plugins/demo',
      ref_name: 'releases/v2',
    }).success
  ).toBe(true))
