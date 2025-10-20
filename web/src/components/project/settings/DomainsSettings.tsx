import { CustomDomainResponse, ProjectResponse } from '@/api/client'
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

  const { data: customDomains, refetch: refetchCustomDomains } = useQuery({
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
    <div>
      <div className="flex items-center justify-between mb-4">
        <h2 className="text-lg font-semibold">Domains</h2>
        <Button onClick={() => setIsAddDialogOpen(true)}>Add Domain</Button>
      </div>

      <p className="text-sm text-muted-foreground mb-6">
        Configure domains for your project. Each domain can be assigned to a
        specific environment and optionally set up with redirects.
      </p>

      {customDomains && customDomains?.domains?.length > 0 ? (
        <div className="space-y-4">
          {customDomains.domains.map((domain) => (
            <div
              key={domain.id}
              className="flex items-center justify-between p-4 rounded-lg border"
            >
              <div>
                <p className="font-medium">{domain.domain}</p>
                {domain.environment && (
                  <p className="text-sm text-muted-foreground">
                    Environment: {domain.environment.slug}
                  </p>
                )}
                {domain.redirect_to && (
                  <p className="text-sm text-muted-foreground">
                    Redirects to: {domain.redirect_to} ({domain.status_code})
                  </p>
                )}
              </div>
              <DropdownMenu>
                <DropdownMenuTrigger>
                  <Button variant="ghost" size="icon">
                    <EllipsisVertical className="h-4 w-4" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
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
