import { ExternalServiceInfo } from '@/api/client/types.gen'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { EditServiceForm } from './EditServiceForm'

interface EditServiceDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  service: ExternalServiceInfo
  onSuccess: () => void
}

export function EditServiceDialog({
  open,
  onOpenChange,
  service,
  onSuccess,
}: EditServiceDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>Edit {service.name}</DialogTitle>
        </DialogHeader>
        <EditServiceForm
          service={service}
          onCancel={() => onOpenChange(false)}
          onSuccess={() => {
            onOpenChange(false)
            onSuccess()
          }}
        />
      </DialogContent>
    </Dialog>
  )
}
