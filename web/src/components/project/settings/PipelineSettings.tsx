// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client'
import { BuilderNodesSettings } from '@/components/settings/BuilderNodesSettings'
import { usePageTitle } from '@/hooks/usePageTitle'

export function PipelineSettings({ project }: { project: ProjectResponse }) {
  usePageTitle(`Pipelines · ${project.name}`)
  return <BuilderNodesSettings key={project.id} projectId={project.id} />
}
