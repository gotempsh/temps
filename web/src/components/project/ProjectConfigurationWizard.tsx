import { ProjectConfigurator, ProjectFormValues } from './ProjectConfigurator'
import { RepositoryResponse } from '@/api/client/types.gen'
import { Dialog, DialogContent } from '@/components/ui/dialog'

interface ProjectConfigurationWizardProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  repository: RepositoryResponse
  connectionId: number
  branches?: any[]
  onSubmit: (data: ProjectFormValues) => Promise<void>
  mode: 'onboarding' | 'import'
}

export function ProjectConfigurationWizard({
  open,
  onOpenChange,
  repository,
  connectionId,
  branches,
  onSubmit,
}: ProjectConfigurationWizardProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-4xl max-h-[90vh] overflow-y-auto p-0">
        <div className="p-6">
          <ProjectConfigurator
            repository={repository}
            connectionId={connectionId}
            branches={branches}
            mode="wizard"
            onSubmit={async (data) => {
              await onSubmit(data)
              onOpenChange(false)
            }}
            onCancel={() => onOpenChange(false)}
          />
        </div>
      </DialogContent>
    </Dialog>
  )
}
