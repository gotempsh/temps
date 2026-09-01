// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { resolveWorkspaceSource } from './workspace-source.js'

/**
 * Only the branches that resolve without touching the network or a TTY:
 * the explicit `--git-url` / `--tarball-url` paths and the argument
 * validation that must fire before any lookup. The project / --repo /
 * inference branches need a live API and are covered by the server-side
 * tests plus manual verification.
 */
describe('resolveWorkspaceSource', () => {
  test('an explicit git url passes straight through', async () => {
    const resolved = await resolveWorkspaceSource({
      gitUrl: 'https://github.com/org/repo.git',
      branch: 'feature/x',
    })
    expect(resolved.source).toEqual({
      type: 'git',
      url: 'https://github.com/org/repo.git',
      revision: 'feature/x',
    })
    expect(resolved.projectId).toBeUndefined()
    expect(resolved.origin).toContain('feature/x')
  })

  test('--git-rev still works as an alias for --branch', async () => {
    const resolved = await resolveWorkspaceSource({
      gitUrl: 'https://github.com/org/repo.git',
      gitRev: 'v1.2.3',
    })
    expect(resolved.source?.revision).toBe('v1.2.3')
  })

  test('--branch wins over --git-rev when both are given', async () => {
    const resolved = await resolveWorkspaceSource({
      gitUrl: 'https://github.com/org/repo.git',
      branch: 'main',
      gitRev: 'v1.2.3',
    })
    expect(resolved.source?.revision).toBe('main')
  })

  test('a git connection id is forwarded so private repos clone', async () => {
    const resolved = await resolveWorkspaceSource({
      gitUrl: 'https://github.com/org/private.git',
      gitConnection: '7',
    })
    expect(resolved.source?.git_connection_id).toBe(7)
  })

  test('a tarball url resolves without any git lookup', async () => {
    const resolved = await resolveWorkspaceSource({
      tarballUrl: 'https://example.com/snapshot.tar.gz',
    })
    expect(resolved.source).toEqual({
      type: 'tarball',
      url: 'https://example.com/snapshot.tar.gz',
    })
  })

  test('git and tarball sources are mutually exclusive', async () => {
    await expect(
      resolveWorkspaceSource({
        gitUrl: 'https://github.com/org/repo.git',
        tarballUrl: 'https://example.com/snapshot.tar.gz',
      }),
    ).rejects.toThrow('mutually exclusive')
  })

  test('a connection and inline credentials are mutually exclusive', async () => {
    await expect(
      resolveWorkspaceSource({
        gitUrl: 'https://github.com/org/repo.git',
        gitConnection: '7',
        gitUsername: 'x-access-token',
        gitPassword: 'ghp_secret',
      }),
    ).rejects.toThrow('mutually exclusive')
  })

  test('inline credentials must be given as a pair', async () => {
    await expect(
      resolveWorkspaceSource({
        gitUrl: 'https://github.com/org/repo.git',
        gitUsername: 'x-access-token',
      }),
    ).rejects.toThrow('must be provided together')
  })

  test('a non-positive depth is rejected before the sandbox is created', async () => {
    await expect(
      resolveWorkspaceSource({
        gitUrl: 'https://github.com/org/repo.git',
        gitDepth: '0',
      }),
    ).rejects.toThrow('positive integer')
  })

  // The point of --repo is that it takes what people read off a repo page.
  // A bare name or a full URL are the two things they'll actually type, and
  // both must say what to do instead rather than 404 against the provider.
  test.each(['repo', 'https://github.com/org/repo', 'org/repo/extra'])(
    'rejects --repo %p with guidance',
    async (value) => {
      await expect(resolveWorkspaceSource({ repo: value })).rejects.toThrow(
        'owner/name',
      )
    },
  )
})
