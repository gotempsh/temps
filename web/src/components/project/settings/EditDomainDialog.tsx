import { CustomDomainResponse, ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  listDomainsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { useQuery } from '@tanstack/react-query'
import { DomainForm } from './DomainForm'

interface EditDomainDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  project: ProjectResponse
  domain: CustomDomainResponse | undefined
  onSuccess: () => void
}

export function EditDomainDialog({
  open,
  onOpenChange,
  project,
  domain,
  onSuccess,
}: EditDomainDialogProps) {
  const { data: domains } = useQuery({
    ...listDomainsOptions({}),
  })

  const { data: environments } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[600px]">
        <DialogHeader>
          <DialogTitle>Edit Domain</DialogTitle>
        </DialogHeader>
        {environments && domains?.domains && domain && (
          <DomainForm
            project_id={project.id}
            environments={environments}
            domains={domains.domains.map((domain) => ({
              id: domain.id.toString(),
              domain: domain.domain,
            }))}
            onSuccess={onSuccess}
            onCancel={() => onOpenChange(false)}
            initialData={domain}
          />
        )}
      </DialogContent>
    </Dialog>
  )
}
