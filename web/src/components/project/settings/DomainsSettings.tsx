// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  CustomDomainResponse,
  ProjectResponse,
  type DomainDeliveryBindingResponse,
} from '@/api/client'
import {
  deleteCustomDomainMutation,
  listCustomDomainsForProjectOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription } from '@/components/ui/alert'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { Skeleton } from '@/components/ui/skeleton'
import { DomainDeliveryBindings } from '@/components/domains/DomainDeliveryBindings'
import { DomainDeliverySetup } from '@/components/domains/DomainDeliverySetup'
import { ProjectDeliverySettings } from '@/components/domains/ProjectDeliverySettings'
import { deliveryError } from '@/components/domains/delivery-errors'
import { HostnameTunnelStatus } from '@/components/domains/TunnelHostnames'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import { KbdBadge } from '@/components/ui/kbd-badge'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { useMutation, useQuery } from '@tanstack/react-query'
import { ChevronDown, EllipsisVertical, Globe } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { EnvironmentReachabilityList } from '@/components/reachability/EnvironmentReachabilityList'
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
  const [domainToDelete, setDomainToDelete] =
    useState<CustomDomainResponse | null>(null)
  const [deliveryOpen, setDeliveryOpen] = useState(false)
  // Bumped per opening so the always-mounted dialog starts from a fresh form
  // and preview for each target, without conditional mounting.
  const [deliverySession, setDeliverySession] = useState(0)
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
    setDeliverySession((session) => session + 1)
    setDeliveryOpen(true)
  }

  useKeyboardShortcut({
    key: 'n',
    callback: () => setIsAddDialogOpen(true),
  })

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
  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <h2 className="text-lg font-semibold">Domains</h2>
        <div className="flex flex-wrap gap-2">
          <Button onClick={() => setIsAddDialogOpen(true)}>
            Add domain
            <KbdBadge keys={['N']} className="ml-2 hidden sm:inline-flex" />
          </Button>
        </div>
      </div>

      <p className="text-sm text-muted-foreground mb-6">
        Configure domains for your service. Each domain can be assigned to a
        specific environment and optionally set up with redirects.
      </p>

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
                <HostnameTunnelStatus hostname={domain.domain} />
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
                    onClick={() => setDomainToDelete(domain)}
                  >
                    Delete
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          ))}
        </div>
      ) : (
        <EmptyState
          icon={Globe}
          title="No domains configured yet"
          description="Add a domain to get started."
          action={
            <Button onClick={() => setIsAddDialogOpen(true)}>
              Add Domain
              <KbdBadge keys={['N']} className="ml-2 hidden sm:inline-flex" />
            </Button>
          }
        />
      )}

      {/* Domains lead the page (DESIGN.md); each environment's reachability
          follows them. An app hosted elsewhere is never served by Temps'
          ingress. */}
      {project.hosting !== 'external' && project.source_type !== 'external' && (
        <EnvironmentReachabilityList
          projectId={project.id}
          projectSlug={project.slug}
        />
      )}

      <Collapsible className="border-t pt-6">
        <CollapsibleTrigger asChild>
          <Button variant="ghost" className="group w-full justify-between px-0">
            DNS and CDN settings
            <ChevronDown className="h-4 w-4 transition-transform group-data-[state=open]:rotate-180" />
          </Button>
        </CollapsibleTrigger>
        <p className="text-sm text-muted-foreground">
          Optionally manage DNS records and deliver traffic through Cloudflare
          or bunny.net.
        </p>
        <CollapsibleContent
          role="region"
          aria-label="DNS and CDN settings"
          className="space-y-6 pt-4"
        >
          <Button variant="outline" onClick={() => configureDelivery()}>
            Configure delivery
          </Button>
          <ProjectDeliverySettings projectId={project.id} />
          <DomainDeliveryBindings
            projectId={project.id}
            onConfigure={configureDelivery}
          />
        </CollapsibleContent>
      </Collapsible>

      <AddDomainDialog
        open={isAddDialogOpen}
        onOpenChange={setIsAddDialogOpen}
        project={project}
        onSuccess={handleAddSuccess}
      />

      <DomainDeliverySetup
        key={deliverySession}
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

      <EditDomainDialog
        open={isEditDialogOpen}
        onOpenChange={setIsEditDialogOpen}
        project={project}
        domain={editingDomain}
        onSuccess={handleEditSuccess}
      />

      <ConfirmDeleteDialog
        open={domainToDelete !== null}
        onOpenChange={(open) => {
          if (!open) setDomainToDelete(null)
        }}
        resourceType="custom domain"
        resourceName={domainToDelete?.domain ?? ''}
        consequences={`Requests to this domain stop reaching ${project.slug}, and any delivery bindings and redirects configured on it are removed.`}
        onConfirm={() => {
          if (domainToDelete) handleDelete(domainToDelete.id)
          setDomainToDelete(null)
        }}
      />
    </div>
  )
}
