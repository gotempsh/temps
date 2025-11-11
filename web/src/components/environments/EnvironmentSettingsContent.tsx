import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { EnvironmentDetail } from '../project/settings/environments/EnvironmentDetail'

interface EnvironmentSettingsContentProps {
  environment: EnvironmentResponse
  project: ProjectResponse
  environmentId: string
}

export function EnvironmentSettingsContent({
  environment,
  project,
  environmentId,
}: EnvironmentSettingsContentProps) {
  return (
    <div className="space-y-6">
      <EnvironmentDetail
        project={project}
        environmentId={parseInt(environmentId)}
        initialEnvironment={environment}
        key={environment.id}
      />
    </div>
  )
}
