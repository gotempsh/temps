// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client'
import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { ProjectCard } from './ProjectCard'

const project = {
  id: 1,
  name: 'Example',
  slug: 'example',
  preset: null,
  last_deployment: '2026-09-02T12:00:00Z',
} as unknown as ProjectResponse

describe('ProjectCard deployment state', () => {
  for (const layout of ['compact', 'dense', 'wide'] as const) {
    test(`${layout} layout uses the latest deployment status`, () => {
      const markup = renderToStaticMarkup(
        <MemoryRouter>
          <ProjectCard
            project={project}
            layout={layout}
            latestDeploymentMedia={{ latest_attempt_status: 'failed' }}
          />
        </MemoryRouter>
      )

      expect(markup).toContain('Last attempt')
      expect(markup).not.toContain('>Deployed<')
    })
  }

  test('shows explicit loading and unavailable states for deployment metadata', () => {
    const loading = renderToStaticMarkup(
      <MemoryRouter>
        <ProjectCard project={project} latestDeploymentMediaLoading />
      </MemoryRouter>
    )
    const unavailable = renderToStaticMarkup(
      <MemoryRouter>
        <ProjectCard project={project} latestDeploymentMediaError />
      </MemoryRouter>
    )

    expect(loading).not.toContain('Last attempt')
    expect(unavailable).toContain('Unavailable')
    expect(unavailable).not.toContain('Last attempt')
  })
})
