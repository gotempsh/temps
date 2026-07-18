import {
  deleteOidcProviderMutation,
  listOidcProvidersOptions,
  listOidcProvidersQueryKey,
  updateOidcProviderMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { OidcProviderResponse } from '@/api/client/types.gen'
import { problemMessage } from '@/components/settings/oidc-provider-constants'
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
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
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
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
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
import { Link } from 'react-router-dom'
import { SiAuth0, SiGoogle, SiKeycloak, SiOkta } from 'react-icons/si'
import { toast } from 'sonner'

function providerIcon(
  template?: string,
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
    case 'generic':
    default:
      return Lock
  }
}

function formatTemplate(template?: string): string {
  if (!template) return 'Generic'
  if (template === 'azure-ad') return 'Azure AD'
  if (template === 'generic') return 'Generic'
  return template.charAt(0).toUpperCase() + template.slice(1)
}

export function AuthSettingsPage() {
  usePageTitle('Authentication')
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()
  const [deleteTarget, setDeleteTarget] = useState<OidcProviderResponse | null>(
    null,
  )

  const providersQuery = useQuery(listOidcProvidersOptions())

  const updateProvider = useMutation({
    ...updateOidcProviderMutation(),
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: listOidcProvidersQueryKey(),
      })
    },
    onError: (error) => {
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
  const error = providersQuery.error
    ? problemMessage(providersQuery.error, 'Failed to load authentication settings')
    : null

  const handleToggle = (provider: OidcProviderResponse, enabled: boolean) => {
    updateProvider.mutate({
      path: { provider_id: provider.id },
      body: { enabled },
    })
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">
            Authentication
          </h1>
          <p className="text-sm text-muted-foreground">
            OIDC provider configuration. Add as many providers as you need —
            each one shows up as its own button on the login screen when enabled.
          </p>
        </div>
        <Button asChild>
          <Link to="/settings/auth/new">
            <Plus className="mr-2 h-4 w-4" />
            <span className="hidden sm:inline">Add SSO Provider</span>
            <span className="sm:hidden">Add</span>
          </Link>
        </Button>
      </div>

      {error && (
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertTitle>Could not load settings</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      {providersQuery.isLoading ? (
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
                <Link to="/settings/auth/new">Add SSO Provider</Link>
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
                          Configure
                        </Link>
                      </DropdownMenuItem>
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

      <AlertDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete SSO provider?</AlertDialogTitle>
            <AlertDialogDescription>
              Users that signed in via{' '}
              <span className="font-medium">{deleteTarget?.name}</span> will no
              longer be able to use SSO. Their Temps accounts stay intact and
              can fall back to password login.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={deleteProvider.isPending}
              onClick={() => {
                if (deleteTarget) {
                  deleteProvider.mutate({
                    path: { provider_id: deleteTarget.id },
                  })
                }
              }}
            >
              {deleteProvider.isPending ? 'Deleting…' : 'Delete provider'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
