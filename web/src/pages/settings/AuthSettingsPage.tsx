// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  deleteOidcProviderMutation,
  listOidcProvidersOptions,
  listOidcProvidersQueryKey,
  updateOidcProviderMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { OidcProviderResponse } from '@/api/client/types.gen'
import { problemMessage } from '@/components/settings/oidc-provider-constants'
import { PageHeader } from '@/components/layout/PageContainer'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { ReadFailure } from '@/components/ui/read-failure'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ChevronRight,
  Cloud,
  EllipsisVertical,
  KeyRound,
  Lock,
  Plus,
  Trash2,
} from 'lucide-react'
import type { ComponentType, SVGProps } from 'react'
import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { SiAuth0, SiGoogle, SiKeycloak, SiOkta } from 'react-icons/si'
import { toast } from 'sonner'

function providerIcon(
  template?: string
): ComponentType<SVGProps<SVGSVGElement>> {
  switch (template) {
    case 'keycloak':
      return SiKeycloak
    case 'okta':
      return SiOkta
    case 'auth0':
      return SiAuth0
    case 'google':
      return SiGoogle
    case 'azure-ad':
      return Cloud
    case 'temps_cloud':
      return Cloud
    case 'generic':
    default:
      return Lock
  }
}

function formatTemplate(template?: string): string {
  if (!template) return 'Generic'
  if (template === 'azure-ad') return 'Azure AD'
  if (template === 'temps_cloud') return 'Temps Cloud'
  if (template === 'generic') return 'Generic'
  return template.charAt(0).toUpperCase() + template.slice(1)
}

/**
 * ADR-045 §4: tooltip shown on the disabled edit/delete controls for the
 * provider Temps Cloud provisions for console access. Points at the one
 * place that actually controls this row instead of leaving the operator to
 * guess why the usual controls are greyed out.
 */
const MANAGED_BY_CLOUD_TOOLTIP =
  'Managed by Temps Cloud. Turn off "Console access through Temps Cloud" on the Temps Cloud settings page to remove it.'

export function AuthSettingsPage() {
  usePageTitle('Authentication')
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const [deleteTarget, setDeleteTarget] = useState<OidcProviderResponse | null>(
    null
  )

  const providersQuery = useQuery(listOidcProvidersOptions())

  const updateProvider = useMutation({
    ...updateOidcProviderMutation(),
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: listOidcProvidersQueryKey(),
      })
    },
    onError: (error, variables) => {
      if (
        handleSensitiveActionError(error, () =>
          updateProvider.mutate(variables)
        )
      ) {
        return
      }
      toast.error(problemMessage(error, 'Failed to update SSO provider'))
    },
  })

  const deleteProvider = useMutation({
    ...deleteOidcProviderMutation(),
    onSuccess: async () => {
      toast.success('SSO provider removed')
      setDeleteTarget(null)
      await queryClient.invalidateQueries({
        queryKey: listOidcProvidersQueryKey(),
      })
    },
    onError: (error) => {
      setDeleteTarget(null)
      toast.error(problemMessage(error, 'Failed to delete SSO provider'))
    },
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Authentication' },
    ])
  }, [setBreadcrumbs])

  const providers = providersQuery.data ?? []
  // The "no providers" empty state is a statement about the installation, so
  // it needs a read that succeeded. A refused or failed read knows nothing.
  const providersUnknown =
    providersQuery.isError && providersQuery.data === undefined

  const handleToggle = (provider: OidcProviderResponse, enabled: boolean) => {
    updateProvider.mutate({
      path: { provider_id: provider.id },
      body: { enabled },
    })
  }

  return (
    <div className="space-y-6">
      {verificationDialog}
      <PageHeader
        title="Authentication"
        description="OIDC provider configuration. Add as many providers as you need — each one shows up as its own button on the login screen when enabled."
        actions={
          !providersQuery.isLoading && providers.length > 0 ? (
            <Button asChild>
              <Link to="/settings/auth/new">
                <Plus className="mr-2 h-4 w-4" />
                <span className="hidden sm:inline">Add SSO provider</span>
                <span className="sm:hidden">Add</span>
              </Link>
            </Button>
          ) : undefined
        }
      />

      {providersQuery.isError && (
        <ReadFailure
          resource="SSO providers"
          error={providersQuery.error}
          cached={!providersUnknown}
          onRetry={() => void providersQuery.refetch()}
          retrying={providersQuery.isFetching}
        />
      )}

      {providersUnknown ? null : providersQuery.isLoading ? (
        <div className="space-y-2">
          {[0, 1, 2].map((idx) => (
            <Skeleton key={idx} className="h-14 w-full rounded-md" />
          ))}
        </div>
      ) : providers.length === 0 ? (
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <KeyRound className="h-5 w-5" />
              SSO providers
            </CardTitle>
            <CardDescription>
              Password login keeps working alongside any SSO provider you add.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <div className="rounded-lg border border-dashed p-8 text-center">
              <KeyRound className="mx-auto mb-3 h-8 w-8 text-muted-foreground" />
              <p className="text-sm font-medium">No SSO providers configured</p>
              <p className="mt-1 text-sm text-muted-foreground">
                Connect Okta, Auth0, Keycloak, Azure AD, or any OIDC-compatible
                IdP.
              </p>
              <Button asChild className="mt-4">
                <Link to="/settings/auth/new">Add SSO provider</Link>
              </Button>
            </div>
          </CardContent>
        </Card>
      ) : (
        <div className="divide-y rounded-md border">
          {providers.map((provider) => {
            const Icon = providerIcon(provider.template)
            const togglingThis =
              updateProvider.isPending &&
              updateProvider.variables?.path?.provider_id === provider.id
            return (
              <div
                key={provider.id}
                className="flex items-center gap-3 px-3 py-2.5"
              >
                <Link
                  to={`/settings/auth/providers/${provider.id}`}
                  className="flex min-w-0 flex-1 items-center gap-3 hover:opacity-80"
                  aria-label={`Edit ${provider.name}`}
                >
                  <Icon
                    className="h-5 w-5 shrink-0 text-muted-foreground"
                    aria-hidden="true"
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="truncate text-sm font-medium">
                        {provider.name}
                      </span>
                      <Badge variant="outline" className="shrink-0">
                        {formatTemplate(provider.template)}
                      </Badge>
                      {provider.managed_by_cloud && (
                        <Badge className="shrink-0 gap-1">
                          <Cloud className="h-3 w-3" aria-hidden="true" />
                          Managed by Temps Cloud
                        </Badge>
                      )}
                      {!provider.enabled && (
                        <Badge variant="secondary" className="shrink-0">
                          Disabled
                        </Badge>
                      )}
                    </div>
                    <p className="truncate text-xs text-muted-foreground">
                      {provider.issuer_url}
                    </p>
                  </div>
                </Link>
                <div className="flex shrink-0 items-center gap-2">
                  {provider.managed_by_cloud ? (
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <span tabIndex={0}>
                          <Switch
                            checked={provider.enabled}
                            disabled
                            aria-label={`${provider.name} is managed by Temps Cloud`}
                          />
                        </span>
                      </TooltipTrigger>
                      <TooltipContent>
                        {MANAGED_BY_CLOUD_TOOLTIP}
                      </TooltipContent>
                    </Tooltip>
                  ) : (
                    <Switch
                      checked={provider.enabled}
                      disabled={togglingThis}
                      onCheckedChange={(checked) =>
                        handleToggle(provider, checked)
                      }
                      aria-label={
                        provider.enabled
                          ? `Disable ${provider.name}`
                          : `Enable ${provider.name}`
                      }
                    />
                  )}
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-8 w-8"
                        aria-label="Provider actions"
                      >
                        <EllipsisVertical className="h-4 w-4" />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem asChild>
                        <Link to={`/settings/auth/providers/${provider.id}`}>
                          {provider.managed_by_cloud ? 'View' : 'Configure'}
                        </Link>
                      </DropdownMenuItem>
                      {provider.managed_by_cloud ? (
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <span>
                              <DropdownMenuItem
                                disabled
                                className="text-destructive focus:text-destructive"
                              >
                                <Trash2 className="mr-2 h-4 w-4" />
                                Delete
                              </DropdownMenuItem>
                            </span>
                          </TooltipTrigger>
                          <TooltipContent side="left">
                            {MANAGED_BY_CLOUD_TOOLTIP}
                          </TooltipContent>
                        </Tooltip>
                      ) : (
                        <DropdownMenuItem
                          className="text-destructive focus:text-destructive"
                          onSelect={(event) => {
                            event.preventDefault()
                            setDeleteTarget(provider)
                          }}
                        >
                          <Trash2 className="mr-2 h-4 w-4" />
                          Delete
                        </DropdownMenuItem>
                      )}
                    </DropdownMenuContent>
                  </DropdownMenu>
                  <Link
                    to={`/settings/auth/providers/${provider.id}`}
                    className="text-muted-foreground hover:text-foreground"
                    aria-label={`Open ${provider.name}`}
                  >
                    <ChevronRight className="h-4 w-4" />
                  </Link>
                </div>
              </div>
            )
          })}
        </div>
      )}

      <ConfirmDeleteDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null)
        }}
        resourceType="SSO provider"
        resourceName={deleteTarget?.name ?? ''}
        consequences="Users that signed in through it will no longer be able to use SSO. Their Temps accounts stay intact and can fall back to password login."
        confirmLabel="Delete provider"
        isPending={deleteProvider.isPending}
        onConfirm={() => {
          if (deleteTarget) {
            deleteProvider.mutate({
              path: { provider_id: deleteTarget.id },
            })
          }
        }}
      />
    </div>
  )
}
