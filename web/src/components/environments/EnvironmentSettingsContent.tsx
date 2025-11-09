import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { EnvironmentDetail } from '../project/settings/environments/EnvironmentDetail'

interface EnvironmentSettingsContentProps {
  environment: EnvironmentResponse
  projectId: string
  environmentId: string
}

export function EnvironmentSettingsContent({
  environment,
  projectId,
  environmentId,
}: EnvironmentSettingsContentProps) {
  // Create a minimal project object for the EnvironmentDetail component
  const project: ProjectResponse = {
    id: parseInt(projectId),
    slug: projectId, // This will be the numeric ID as a string
  } as ProjectResponse

  return (
    <div className="space-y-6">
      <EnvironmentDetail
        project={project}
        environmentId={parseInt(environmentId)}
      />
    </div>
  )
}
