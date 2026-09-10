import {
  CustomDomainResponse,
  ProjectResponse,
  type DomainDeliveryBindingResponse,
} from '@/api/client'
import {
  deleteCustomDomainMutation,
  listCustomDomainsForProjectOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Skeleton } from '@/components/ui/skeleton'
import { DomainDeliveryBindings } from '@/components/domains/DomainDeliveryBindings'
import { DomainDeliverySetup } from '@/components/domains/DomainDeliverySetup'
import { ProjectDeliverySettings } from '@/components/domains/ProjectDeliverySettings'
import { deliveryError } from '@/components/domains/delivery-errors'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { useMutation, useQuery } from '@tanstack/react-query'
import { EllipsisVertical } from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'
import { AddDomainDialog } from './AddDomainDialog'
import { EditDomainDialog } from './EditDomainDialog'

interface DomainsSettingsProps {
  project: ProjectResponse
}

export function DomainsSettings({ project }: DomainsSettingsProps) {
  const [isAddDialogOpen, setIsAddDialogOpen] = useState(false)
  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)
  const [editingDomain, setEditingDomain] = useState<
    CustomDomainResponse | undefined
  >()
  const [domainToDelete, setDomainToDelete] = useState<number | null>(null)
  const [deliveryOpen, setDeliveryOpen] = useState(false)
  const [deliveryTarget, setDeliveryTarget] = useState<{
    hostname: string
    environmentId?: number
    binding?: DomainDeliveryBindingResponse
  }>({ hostname: '' })
  const configureDelivery = (
    hostname = '',
    environmentId?: number,
    binding?: DomainDeliveryBindingResponse
  ) => {
    setDeliveryTarget({ hostname, environmentId, binding })
    setDeliveryOpen(true)
  }

  const {
    data: customDomains,
    refetch: refetchCustomDomains,
    isPending,
    error,
  } = useQuery({
    ...listCustomDomainsForProjectOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  const deleteDomain = useMutation({
    ...deleteCustomDomainMutation(),
    meta: {
      errorTitle: 'Failed to delete custom domain',
    },
    onSuccess: () => {
      toast.success('Domain deleted successfully')
      refetchCustomDomains()
    },
  })

  const handleAddSuccess = () => {
    setIsAddDialogOpen(false)
    refetchCustomDomains()
  }

  const handleEditSuccess = () => {
    setIsEditDialogOpen(false)
    setEditingDomain(undefined)
    refetchCustomDomains()
  }

  const handleDelete = (domainId: number) => {
    deleteDomain.mutate({
      path: {
        project_id: project.id,
        domain_id: domainId,
      },
    })
  }
  const deleteDialogOpen = useMemo(
    () => domainToDelete !== null,
    [domainToDelete]
  )
  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <h2 className="text-lg font-semibold">Domains</h2>
        <div className="flex flex-wrap gap-2">
          <Button variant="outline" onClick={() => setIsAddDialogOpen(true)}>
            Add domain manually
          </Button>
          <Button onClick={() => configureDelivery()}>
            Configure delivery
          </Button>
        </div>
      </div>

      <p className="text-sm text-muted-foreground mb-6">
        Configure domains for your project. Each domain can be assigned to a
        specific environment and optionally set up with redirects.
      </p>

      <ProjectDeliverySettings projectId={project.id} />
      <DomainDeliveryBindings
        projectId={project.id}
        onConfigure={configureDelivery}
      />
      <h3 className="font-semibold">Project routes</h3>

      {isPending ? (
        <Skeleton className="h-24 w-full" />
      ) : error ? (
        <Alert variant="destructive">
          <AlertDescription>
            {deliveryError(error)}{' '}
            <Button variant="link" onClick={() => refetchCustomDomains()}>
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      ) : customDomains && customDomains?.domains?.length > 0 ? (
        <div className="space-y-4">
          {customDomains.domains.map((domain) => (
            <div
              key={domain.id}
              className="flex items-center justify-between gap-4 p-4 rounded-lg border"
            >
              <div className="min-w-0">
                <p className="break-all font-medium">{domain.domain}</p>
                {domain.environment && (
                  <p className="text-sm text-muted-foreground">
                    Environment: {domain.environment.slug}
                  </p>
                )}
                {domain.service_name && (
                  <p className="text-sm text-muted-foreground">
                    Service: {domain.service_name}
                  </p>
                )}
                {domain.redirect_to && (
                  <p className="text-sm text-muted-foreground">
                    Redirects to: {domain.redirect_to} ({domain.status_code})
                  </p>
                )}
              </div>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={`Actions for ${domain.domain}`}
                  >
                    <EllipsisVertical className="h-4 w-4" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  <DropdownMenuItem
                    onClick={() =>
                      configureDelivery(domain.domain, domain.environment?.id)
                    }
                  >
                    Configure delivery
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onClick={() => {
                      setEditingDomain(domain)
                      setIsEditDialogOpen(true)
                    }}
                  >
                    Edit
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    className="text-destructive"
                    onClick={() => setDomainToDelete(domain.id)}
                  >
                    Delete
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          ))}
        </div>
      ) : (
        <div className="text-sm text-muted-foreground">
          No domains configured yet. Add a domain to get started.
        </div>
      )}

      <AddDomainDialog
        open={isAddDialogOpen}
        onOpenChange={setIsAddDialogOpen}
        project={project}
        onSuccess={handleAddSuccess}
      />

      {deliveryOpen && (
        <DomainDeliverySetup
          projectId={project.id}
          open={deliveryOpen}
          onOpenChange={(open) => {
            setDeliveryOpen(open)
            if (!open) refetchCustomDomains()
          }}
          initialHostname={deliveryTarget.hostname}
          initialEnvironmentId={deliveryTarget.environmentId}
          initialBinding={deliveryTarget.binding}
        />
      )}

      <EditDomainDialog
        open={isEditDialogOpen}
        onOpenChange={setIsEditDialogOpen}
        project={project}
        domain={editingDomain}
        onSuccess={handleEditSuccess}
      />

      <AlertDialog
        open={deleteDialogOpen}
        onOpenChange={(open) => !open && setDomainToDelete(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Are you sure?</AlertDialogTitle>
            <AlertDialogDescription>
              This action cannot be undone. This will permanently delete the
              domain from your project.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (domainToDelete) handleDelete(domainToDelete)
                setDomainToDelete(null)
              }}
            >
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
