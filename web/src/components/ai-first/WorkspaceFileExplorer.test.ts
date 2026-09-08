// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { workspaceFileLanguage } from './workspace-file-language'

describe('workspace file syntax highlighting', () => {
  test('maps common project files to Shiki languages', () => {
    expect(workspaceFileLanguage('projects/app/src/page.tsx')).toBe('tsx')
    expect(workspaceFileLanguage('projects/app/tsconfig.json')).toBe('json')
    expect(workspaceFileLanguage('projects/app/Dockerfile')).toBe('dockerfile')
    expect(workspaceFileLanguage('projects/app/README.md')).toBe('markdown')
    expect(workspaceFileLanguage('projects/app/Cargo.toml')).toBe('toml')
    expect(workspaceFileLanguage('projects/app/styles.css')).toBe('css')
  })

  test('falls back to plain text for unknown extensions', () => {
    expect(workspaceFileLanguage('projects/app/LICENSE')).toBe('text')
  })
})
