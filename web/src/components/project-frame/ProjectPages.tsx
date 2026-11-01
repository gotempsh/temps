// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentResponse, ProjectResponse } from '@/api/client'
import { ProjectDeployments } from '@/components/project/ProjectDeployments'
import { ProjectOverview } from '@/components/project/ProjectOverview'
import { EnvironmentsTabsView } from '@/pages/EnvironmentsTabsView'
import { useProjectEnvironment } from './project-environment-context'

/*
 * Project pages fed the environment chosen in the project bar instead of
 * picking one themselves. Each is keyed by the selection so paging and other
 * per-environment state start fresh when the environment changes.
 */

export function ProjectOverviewPage({
  project,
  lastDeployment,
}: {
  project: ProjectResponse
  lastDeployment?: DeploymentResponse
}) {
  const { selection } = useProjectEnvironment()
  return (
    <ProjectOverview
      project={project}
      lastDeployment={lastDeployment}
      environmentId={selection.environment?.id}
    />
  )
}

export function ProjectDeploymentsPage({
  project,
}: {
  project: ProjectResponse
}) {
  const { selection } = useProjectEnvironment()
  const environmentId = selection.all ? undefined : selection.environment?.id
  return (
    <ProjectDeployments
      key={environmentId ?? 'all'}
      project={project}
      environmentId={environmentId}
    />
  )
}

export function ProjectEnvironmentsPage({
  project,
}: {
  project: ProjectResponse
}) {
  const { selection } = useProjectEnvironment()
  return (
    <EnvironmentsTabsView
      project={project}
      selectedEnvironmentId={selection.environment?.id}
    />
  )
}
