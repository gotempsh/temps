// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  LatestWorkspaceRequest,
  nextWorkspaceFileRevision,
  uploadWorkspaceBatches,
  workspaceFileExplorerKey,
} from './workspace-file-operations'

describe('workspace file operation coordination', () => {
  test('only lets the latest deferred preview commit', async () => {
    const requests = new LatestWorkspaceRequest()
    const first = requests.begin()
    const second = requests.begin()

    await Promise.resolve()

    expect(requests.isCurrent(first)).toBe(false)
    expect(requests.isCurrent(second)).toBe(true)
  })

  test('changes identity when the application context changes', () => {
    expect(workspaceFileExplorerKey('application-a')).not.toBe(
      workspaceFileExplorerKey('application-b')
    )
    expect(workspaceFileExplorerKey()).toBe('global-workspace')
  })

  test('invalidates every responsive explorer after a shared mutation', () => {
    const sharedRevision = nextWorkspaceFileRevision(3)

    expect(sharedRevision).toBe(4)
  })

  test('reports completed files when a later upload batch fails', async () => {
    const failure = new Error('second batch failed')
    let calls = 0
    const result = await uploadWorkspaceBatches([[1, 2], [3]], async () => {
      calls += 1
      if (calls === 2) throw failure
    })

    expect(result).toEqual({ completed: 2, error: failure })
  })
})
