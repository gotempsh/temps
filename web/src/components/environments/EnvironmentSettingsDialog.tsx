import { EnvironmentResponse } from '@/api/client'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { EnvironmentSettingsContent } from './EnvironmentSettingsContent'

interface EnvironmentSettingsDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  environment: EnvironmentResponse
  projectId: string
}

export function EnvironmentSettingsDialog({
  open,
  onOpenChange,
  environment,
  projectId,
}: EnvironmentSettingsDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-4xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{environment.name} Settings</DialogTitle>
        </DialogHeader>

        <EnvironmentSettingsContent
          environment={environment}
          projectId={projectId}
          environmentId={environment.id.toString()}
        />
      </DialogContent>
    </Dialog>
  )
}
