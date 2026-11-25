// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageHeader } from '@/components/layout/PageContainer'
import {
  CustomDomainResponse,
  ProjectResponse,
  type DomainDeliveryBindingResponse,
} from '@/api/client'
import {
  deleteCustomDomainMutation,
  listCustomDomainsForProjectOptions,
  listCustomDomainsForProjectQueryKey,
  getEnvironmentReachabilityQueryKey,
  listIngressHostnamesQueryKey,
  listReachabilityRunsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription } from '@/components/ui/alert'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { RefreshButton } from '@/components/ui/refresh-button'
import { queryKeyPrefix } from '@/lib/refresh'
import { Skeleton } from '@/components/ui/skeleton'
import { DomainDeliveryBindings } from '@/components/domains/DomainDeliveryBindings'
import { DomainDeliverySetup } from '@/components/domains/DomainDeliverySetup'
import { ProjectDeliverySettings } from '@/components/domains/ProjectDeliverySettings'
import { deliveryError } from '@/components/domains/delivery-errors'
import { CustomDomainRow } from '@/components/domains/CustomDomainParts'
import { HostnameTunnelMenu } from '@/components/domains/TunnelHostnames'
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
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
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

  const queryClient = useQueryClient()
  // A domain change moves addresses between environments: the reachability
  // the list shows them by is refreshed with the domains, for this project.
  const refreshDomains = () => {
    refetchCustomDomains()
    for (const queryKey of [
      queryKeyPrefix(getEnvironmentReachabilityQueryKey, {
        path: { project_id: project.id },
      }),
      queryKeyPrefix(listReachabilityRunsQueryKey, {
        path: { project_id: project.id },
      }),
    ])
      void queryClient.invalidateQueries({ queryKey })
  }

  const deleteDomain = useMutation({
    ...deleteCustomDomainMutation(),
    meta: {
      errorTitle: 'Failed to delete custom domain',
    },
    onSuccess: () => {
      toast.success('Domain deleted successfully')
      refreshDomains()
    },
  })

  const handleAddSuccess = () => {
    setIsAddDialogOpen(false)
    refreshDomains()
  }

  const handleEditSuccess = () => {
    setIsEditDialogOpen(false)
    setEditingDomain(undefined)
    refreshDomains()
  }

  const handleDelete = (domainId: number) => {
    deleteDomain.mutate({
      path: {
        project_id: project.id,
        domain_id: domainId,
      },
    })
  }
  const domains = customDomains?.domains ?? []
  const servedByTemps =
    project.hosting !== 'external' && project.source_type !== 'external'
  const domainMenu = (domain: CustomDomainResponse) => (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="-my-1 h-8 w-8 shrink-0"
          aria-label={`Actions for ${domain.domain}`}
        >
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
        <DropdownMenuItem
          onClick={() =>
            configureDelivery(domain.domain, domain.environment?.id)
          }
        >
          Configure delivery
        </DropdownMenuItem>
        <HostnameTunnelMenu hostname={domain.domain} />
        <DropdownMenuSeparator />
        <DropdownMenuItem
          className="text-destructive"
          onClick={() => setDomainToDelete(domain)}
        >
          Delete
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )

  return (
    <div className="space-y-6">
      <PageHeader
        title="Domains"
        actions={
          <>
            <RefreshButton
              queryKeys={[
                listCustomDomainsForProjectQueryKey({
                  path: { project_id: project.id },
                }),
                queryKeyPrefix(listIngressHostnamesQueryKey),
                queryKeyPrefix(getEnvironmentReachabilityQueryKey),
                queryKeyPrefix(listReachabilityRunsQueryKey),
                // The managed-delivery rows under DNS and CDN, not its form.
                ['delivery-bindings', project.id],
              ]}
            />
            <Button onClick={() => setIsAddDialogOpen(true)}>
              Add domain
              <KbdBadge keys={['N']} className="ml-2 hidden sm:inline-flex" />
            </Button>
          </>
        }
      />

      {error && (
        <Alert variant="destructive">
          <AlertDescription>
            {deliveryError(error)}{' '}
            <Button variant="link" onClick={() => refetchCustomDomains()}>
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      )}
      {servedByTemps ? (
        // Each custom domain appears once, on the address it is, in its
        // environment's list.
        <EnvironmentReachabilityList
          projectId={project.id}
          projectSlug={project.slug}
          domains={domains}
          domainMenu={domainMenu}
        />
      ) : isPending ? (
        <Skeleton className="h-24 w-full" />
      ) : domains.length > 0 ? (
        // An app hosted elsewhere is never served by Temps' ingress, so its
        // domains have no reachability to show.
        <ul className="divide-y rounded-lg border">
          {domains.map((domain) => (
            <CustomDomainRow
              key={domain.id}
              domain={domain}
              menu={domainMenu}
            />
          ))}
        </ul>
      ) : (
        !error && (
          <EmptyState
            icon={Globe}
            title="No domains configured yet"
            description="Add a domain to get started."
            action={
              <Button onClick={() => setIsAddDialogOpen(true)}>
                Add domain
                <KbdBadge keys={['N']} className="ml-2 hidden sm:inline-flex" />
              </Button>
            }
          />
        )
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
          if (!open) refreshDomains()
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
