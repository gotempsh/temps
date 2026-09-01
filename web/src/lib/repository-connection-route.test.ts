// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  repositoryConnectionBasePath,
  repositoryConnectionPath,
  repositorySelectionPath,
} from './repository-connection-route'

describe('repository connection routes', () => {
  it('builds the connection chooser path', () => {
    expect(repositoryConnectionBasePath('gitlab-compat')).toBe(
      '/projects/gitlab-compat/connect-repository'
    )
  })

  it('builds the repositories path for a connection', () => {
    expect(repositoryConnectionPath('gitlab-compat', 12)).toBe(
      '/projects/gitlab-compat/connect-repository/connections/12'
    )
  })

  it('builds the selected repository path', () => {
    expect(repositorySelectionPath('gitlab-compat', 12, 34)).toBe(
      '/projects/gitlab-compat/connect-repository/connections/12/repositories/34'
    )
  })

  it('encodes project slugs used in URL segments', () => {
    expect(repositoryConnectionBasePath('project name')).toBe(
      '/projects/project%20name/connect-repository'
    )
  })
})
