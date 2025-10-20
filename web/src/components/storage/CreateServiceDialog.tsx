import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { CreateServiceForm } from './CreateServiceForm'
import { CreateServiceResponse, ServiceTypeRoute } from '@/api/client'

interface CreateServiceDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  serviceType: ServiceTypeRoute
  onSuccess: (data: CreateServiceResponse) => void
}

export function CreateServiceDialog({
  open,
  onOpenChange,
  serviceType,
  onSuccess,
}: CreateServiceDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Create {serviceType} Service</DialogTitle>
        </DialogHeader>
        <CreateServiceForm
          serviceType={serviceType}
          onCancel={() => onOpenChange(false)}
          onSuccess={onSuccess}
        />
      </DialogContent>
    </Dialog>
  )
}
