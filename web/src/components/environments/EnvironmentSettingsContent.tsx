// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { EnvironmentDetail } from '../project/settings/environments/EnvironmentDetail'

interface EnvironmentSettingsContentProps {
  environment: EnvironmentResponse
  project: ProjectResponse
  environmentId: string
  onDelete?: () => void
}

export function EnvironmentSettingsContent({
  environment,
  project,
  environmentId,
  onDelete,
}: EnvironmentSettingsContentProps) {
  return (
    <div className="space-y-6">
      <EnvironmentDetail
        project={project}
        environmentId={parseInt(environmentId)}
        initialEnvironment={environment}
        onDelete={onDelete}
        key={environment.id}
      />
    </div>
  )
}
