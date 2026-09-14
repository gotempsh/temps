// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { WorkspaceGitConnections } from './WorkspaceGitConnections'

const base = {
  projects: [{ id: 1, name: 'Website' }],
  bindings: [],
  repositories: [],
  onConnect: () => {},
  onDisconnect: () => {},
}

describe('WorkspaceGitConnections', () => {
  test('unconfigured workspaces keep the feature visible with onboarding', () => {
    const html = renderToStaticMarkup(<WorkspaceGitConnections {...base} />)
    expect(html).toContain('Git connections')
    expect(html).toContain('No eligible repositories')
    expect(html).toContain('href="/git-providers"')
    expect(html).not.toContain('type="password"')
  })
  test('multiple accounts remain explicit even for the same repository name', () => {
    const html = renderToStaticMarkup(
      <WorkspaceGitConnections
        {...base}
        repositories={[
          {
            repositoryId: 10,
            connectionId: 2,
            accountName: 'Personal',
            fullName: 'me/site',
          },
          {
            repositoryId: 11,
            connectionId: 3,
            accountName: 'Work',
            fullName: 'me/site',
          },
        ]}
      />
    )
    expect(html).toContain('value="2:10"')
    expect(html).toContain('value="3:11"')
    expect(html).toContain('Personal · me/site · connection #2')
    expect(html).toContain('Work · me/site · connection #3')
    expect(html).toMatch(/<button[^>]*type="submit"[^>]*disabled/)
  })
  test('configured bindings do not imply files have been published', () => {
    const html = renderToStaticMarkup(
      <WorkspaceGitConnections
        {...base}
        bindings={[
          {
            id: 4,
            projectId: 1,
            connectionId: 2,
            repositoryUrl: 'https://github.com/me/site.git',
            remoteName: 'origin',
            status: 'configured',
          },
          {
            id: 5,
            projectId: 1,
            connectionId: 3,
            repositoryUrl: 'https://git.example/me/site.git',
            remoteName: 'backup',
            status: 'configured',
          },
        ]}
      />
    )
    expect(html).toContain('origin')
    expect(html).toContain('backup')
    expect(html).toContain('does not publish files')
    expect(html).toContain('Disconnect origin from project 1')
  })
  test('loading and failures are exposed accessibly', () => {
    const html = renderToStaticMarkup(
      <WorkspaceGitConnections
        {...base}
        loading
        error="Connection access was revoked"
      />
    )
    expect(html).toContain('role="status"')
    expect(html).toContain('role="alert"')
    expect(html).toContain('Connection access was revoked')
    expect(html).not.toContain('No eligible repositories')
  })
})
