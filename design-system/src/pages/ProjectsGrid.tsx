// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PackageOpen } from 'lucide-react'
import { CardGrid, PageState, ProjectAvatar, Status, fmtRelativeTime, useUrlState } from '@temps-sdk/ds'
import { Input } from '@temps-sdk/ui'
import { PROJECTS, type ProjectFixture } from '../fixtures'

/** A minimal card renderer for the sandbox — not `ProjectCard` (out of scope, see follow-ups). */
function SandboxProjectCard({ project }: { project: ProjectFixture }) {
  return (
    <div className="flex items-start gap-3 rounded-lg border p-4">
      <ProjectAvatar name={project.name} />
      <div className="min-w-0 flex-1 space-y-1">
        <div className="flex items-center justify-between gap-2">
          <span className="truncate font-medium">{project.name}</span>
          <Status tone={project.status} label={project.statusLabel} variant="dot" />
        </div>
        <p className="text-xs text-muted-foreground">
          {project.deployCount} deploys · last {fmtRelativeTime(project.lastDeployedAt)}
        </p>
      </div>
    </div>
  )
}

/**
 * Reference screen for the `CardGrid` template — a "Projects" grid. Mirrors
 * `web/src/pages/Projects.tsx`'s card-grid layout with invented fixtures;
 * `Projects.tsx` itself was not migrated onto this template (batch
 * analytics fetching, first-run onboarding, a migration-source header strip
 * — see design-system-handoff.md follow-ups for why).
 */
export default function ProjectsGrid() {
  const { state, patch } = useUrlState<'q'>()
  const q = state.q?.toLowerCase() ?? ''

  const items = PROJECTS.filter((p) => !q || p.name.includes(q))

  return (
    <CardGrid<ProjectFixture>
      title="Projects"
      description="Manage your projects and their settings."
      toolbar={
        <Input
          placeholder="Filter projects…"
          value={state.q ?? ''}
          onChange={(e) => patch({ q: e.target.value || undefined })}
          className="max-w-sm"
        />
      }
      items={items}
      keyFn={(p) => p.id}
      renderCard={(p) => <SandboxProjectCard project={p} />}
      empty={
        <PageState
          variant="empty"
          icon={PackageOpen}
          title="No projects match that filter"
          description="Clear the filter to see every project."
        />
      }
    />
  )
}
