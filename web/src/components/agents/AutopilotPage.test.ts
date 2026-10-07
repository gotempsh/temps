// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { resolvePath } from 'react-router'

import { autopilotPaths } from './AutopilotPage'

// AutopilotPage renders at /projects/:slug/agents and inside
// Settings → Automation; its links must land on the same page from both.
const MOUNTS = ['/projects/demo/agents', '/projects/demo/settings/automation']

describe('autopilotPaths', () => {
  test('resolve to the agent routes from every mount', () => {
    const paths = autopilotPaths('demo')
    for (const mount of MOUNTS) {
      expect(resolvePath(paths.agent('nightly'), mount).pathname).toBe(
        '/projects/demo/agents/detail/nightly'
      )
      expect(resolvePath(paths.editAgent('nightly'), mount).pathname).toBe(
        '/projects/demo/agents/detail/nightly/edit'
      )
      expect(resolvePath(paths.run(42), mount).pathname).toBe(
        '/projects/demo/agents/42'
      )
    }
  })
})
