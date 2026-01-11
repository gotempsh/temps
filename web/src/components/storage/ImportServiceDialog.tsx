import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { useState } from 'react'
import { ContainerSelector } from './ContainerSelector'
import { ImportServiceForm } from './ImportServiceForm'
import { AvailableContainerInfo } from '@/api/client/types.gen'

interface ImportServiceDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  onSuccess: () => void
}

type Step = 'select-container' | 'enter-details'

export function ImportServiceDialog({
  open,
  onOpenChange,
  onSuccess,
}: ImportServiceDialogProps) {
  const [step, setStep] = useState<Step>('select-container')
  const [selectedContainer, setSelectedContainer] =
    useState<AvailableContainerInfo | null>(null)

  const handleContainerSelected = (container: AvailableContainerInfo) => {
    setSelectedContainer(container)
    setStep('enter-details')
  }

  const handleBack = () => {
    setStep('select-container')
    setSelectedContainer(null)
  }

  const handleSuccess = () => {
    setStep('select-container')
    setSelectedContainer(null)
    onOpenChange(false)
    onSuccess()
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {step === 'select-container'
              ? 'Select Container to Import'
              : `Import ${selectedContainer?.container_name}`}
          </DialogTitle>
        </DialogHeader>

        {step === 'select-container' && (
          <ContainerSelector onContainerSelected={handleContainerSelected} />
        )}

        {step === 'enter-details' && selectedContainer && (
          <ImportServiceForm
            container={selectedContainer}
            onCancel={handleBack}
            onSuccess={handleSuccess}
          />
        )}
      </DialogContent>
    </Dialog>
  )
}
