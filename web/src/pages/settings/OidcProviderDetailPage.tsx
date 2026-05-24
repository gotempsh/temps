import {
  deleteOidcProviderMutation,
  listOidcProviderUsersOptions,
  listOidcProvidersOptions,
  listOidcProvidersQueryKey,
  testOidcProviderMutation,
  updateOidcProviderMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  OidcProviderResponse,
  OidcProviderUserResponse,
  OidcTestConnectionResponse,
} from '@/api/client/types.gen'
import { OidcProviderForm } from '@/components/settings/OidcProviderForm'
import { OidcRoleMappingsCard } from '@/components/settings/OidcRoleMappingsCard'
import {
  getOidcRedirectUri,
  isOidcEditFormValid,
  problemMessage,
  providerToFormValues,
} from '@/components/settings/oidc-provider-constants'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
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
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  KeyRound,
  ShieldCheck,
  Trash2,
  Users as UsersIcon,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'

function formatDate(iso: string): string {
  try {
    return new Date(iso).toLocaleString()
  } catch {
    return iso
  }
}

export function OidcProviderDetailPage() {
  usePageTitle('SSO Provider')
  const params = useParams<{ providerId: string }>()
  const providerId = Number(params.providerId)
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [testResult, setTestResult] =
    useState<OidcTestConnectionResponse | null>(null)
  const [deleteOpen, setDeleteOpen] = useState(false)

  const providersQuery = useQuery(listOidcProvidersOptions())
  const provider: OidcProviderResponse | undefined =
    providersQuery.data?.find((p) => p.id === providerId) ?? undefined

  const usersQuery = useQuery({
    ...listOidcProviderUsersOptions({ path: { provider_id: providerId } }),
    enabled: Number.isFinite(providerId) && providerId > 0,
  })

  const redirectUri = useMemo(() => getOidcRedirectUri(), [])

  const updateProvider = useMutation({
    ...updateOidcProviderMutation(),
    onSuccess: async () => {
      toast.success('SSO provider saved')
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
      await queryClient.invalidateQueries({
        queryKey: listOidcProvidersQueryKey(),
      })
      navigate('/settings/auth')
    },
    onError: (error) => {
      toast.error(problemMessage(error, 'Failed to delete SSO provider'))
    },
  })

  const testProvider = useMutation({
    ...testOidcProviderMutation(),
    onSuccess: (response) => {
      setTestResult(response ?? null)
    },
    onError: (error) => {
      setTestResult({
        success: false,
        message: problemMessage(error, 'Connection test failed'),
      })
    },
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Authentication', href: '/settings/auth' },
      { label: provider?.name ?? 'Provider' },
    ])
  }, [setBreadcrumbs, provider?.name])

  if (!Number.isFinite(providerId) || providerId <= 0) {
    return (
      <div className="space-y-4">
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertTitle>Invalid provider</AlertTitle>
          <AlertDescription>
            The provider ID in the URL is not valid.
          </AlertDescription>
        </Alert>
        <Button variant="outline" onClick={() => navigate('/settings/auth')}>
          <ArrowLeft className="mr-2 h-4 w-4" />
          Back to authentication
        </Button>
      </div>
    )
  }

  if (providersQuery.isLoading) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-96 w-full" />
      </div>
    )
  }

  if (!provider) {
    return (
      <div className="space-y-4">
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertTitle>Provider not found</AlertTitle>
          <AlertDescription>
            The OIDC provider you requested does not exist or was deleted.
          </AlertDescription>
        </Alert>
        <Button variant="outline" onClick={() => navigate('/settings/auth')}>
          <ArrowLeft className="mr-2 h-4 w-4" />
          Back to authentication
        </Button>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="flex items-start gap-3">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate('/settings/auth')}
            aria-label="Back to authentication"
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <div className="flex items-center gap-2">
              <h1 className="text-2xl font-semibold tracking-tight">
                {provider.name}
              </h1>
              <Badge variant="outline">{provider.template}</Badge>
              {!provider.enabled && (
                <Badge variant="secondary">Disabled</Badge>
              )}
            </div>
            <p className="text-sm text-muted-foreground">
              {provider.issuer_url}
            </p>
          </div>
        </div>
      </div>

      <ProviderEditor
        provider={provider}
        redirectUri={redirectUri}
        onSave={(body) =>
          updateProvider.mutate({ path: { provider_id: provider.id }, body })
        }
        saving={updateProvider.isPending}
        onTest={() => {
          setTestResult(null)
          testProvider.mutate({ path: { provider_id: provider.id } })
        }}
        testing={testProvider.isPending}
        onDelete={() => setDeleteOpen(true)}
        deleting={deleteProvider.isPending}
        testResult={testResult}
      />

      <OidcRoleMappingsCard
        providerId={provider.id}
        defaultRole={provider.default_role}
      />

      <UsersForProviderCard
        loading={usersQuery.isLoading}
        error={
          usersQuery.error
            ? problemMessage(usersQuery.error, 'Failed to load users')
            : null
        }
        users={usersQuery.data ?? []}
      />

      <AlertDialog open={deleteOpen} onOpenChange={setDeleteOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete this SSO provider?</AlertDialogTitle>
            <AlertDialogDescription>
              Users that signed in via{' '}
              <span className="font-medium">{provider.name}</span> will no
              longer be able to use SSO. Their Temps accounts stay intact and
              can fall back to password login.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={deleteProvider.isPending}
              onClick={() =>
                deleteProvider.mutate({ path: { provider_id: provider.id } })
              }
            >
              {deleteProvider.isPending ? 'Deleting…' : 'Delete provider'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

function ProviderEditor({
  provider,
  redirectUri,
  onSave,
  saving,
  onTest,
  testing,
  onDelete,
  deleting,
  testResult,
}: {
  provider: OidcProviderResponse
  redirectUri: string
  onSave: (body: {
    name: string
    issuer_url: string
    client_id: string
    client_secret?: string
    scopes: string
    enabled: boolean
    jit_provisioning: boolean
    template: string
    group_claim: string
    role_claim: string
    default_role: string
  }) => void
  saving: boolean
  onTest: () => void
  testing: boolean
  onDelete: () => void
  deleting: boolean
  testResult: OidcTestConnectionResponse | null
}) {
  const [form, setForm] = useState(() => providerToFormValues(provider))

  useEffect(() => {
    setForm(providerToFormValues(provider))
  }, [provider.id])

  const handleSubmit = () => {
    if (!isOidcEditFormValid(form)) {
      toast.error('Fill in name, issuer URL, and client ID')
      return
    }
    onSave({
      name: form.name.trim(),
      issuer_url: form.issuer_url.trim(),
      client_id: form.client_id.trim(),
      ...(form.client_secret.trim().length > 0
        ? { client_secret: form.client_secret }
        : {}),
      scopes: form.scopes.trim(),
      enabled: form.enabled,
      jit_provisioning: form.jit_provisioning,
      template: form.template,
      group_claim: form.group_claim.trim(),
      role_claim: form.role_claim.trim(),
      default_role: form.default_role,
    })
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <KeyRound className="h-5 w-5" />
          Connection
        </CardTitle>
        <CardDescription>
          Update connection settings, then test before enabling on the login
          screen.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <OidcProviderForm
          mode="edit"
          value={form}
          onChange={setForm}
          redirectUri={redirectUri}
          onCancel={() => setForm(providerToFormValues(provider))}
          onSubmit={handleSubmit}
          submitting={saving}
          submitLabel="Save"
          footer={
            <div className="space-y-4">
              <div className="flex flex-wrap gap-2">
                <Button
                  type="button"
                  variant="outline"
                  onClick={onTest}
                  disabled={testing}
                >
                  {testing ? 'Testing…' : 'Test connection'}
                </Button>
                <Button
                  type="button"
                  variant="destructive"
                  onClick={onDelete}
                  disabled={deleting}
                >
                  <Trash2 className="mr-2 h-4 w-4" />
                  Delete provider
                </Button>
              </div>
              {testResult && (
                <Alert variant={testResult.success ? 'default' : 'destructive'}>
                  <AlertTitle>
                    {testResult.success ? 'Connection OK' : 'Connection failed'}
                  </AlertTitle>
                  <AlertDescription>{testResult.message}</AlertDescription>
                </Alert>
              )}
            </div>
          }
        />
      </CardContent>
    </Card>
  )
}

function UsersForProviderCard({
  loading,
  error,
  users,
}: {
  loading: boolean
  error: string | null
  users: OidcProviderUserResponse[]
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <UsersIcon className="h-5 w-5" />
          Users
        </CardTitle>
        <CardDescription>
          Temps accounts that have signed in through this provider. Newly
          provisioned users only appear after their first successful SSO login.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {loading ? (
          <div className="space-y-2">
            {[0, 1, 2].map((idx) => (
              <Skeleton key={idx} className="h-12 w-full rounded-md" />
            ))}
          </div>
        ) : error ? (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Could not load users</AlertTitle>
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        ) : users.length === 0 ? (
          <div className="rounded-lg border border-dashed p-8 text-center">
            <UsersIcon className="mx-auto mb-3 h-8 w-8 text-muted-foreground" />
            <p className="text-sm font-medium">No users yet</p>
            <p className="mt-1 text-sm text-muted-foreground">
              Users will show up here after they sign in for the first time.
            </p>
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full min-w-[640px] text-sm">
              <thead>
                <tr className="text-left text-xs uppercase text-muted-foreground">
                  <th className="px-2 py-2 font-medium">User</th>
                  <th className="px-2 py-2 font-medium">IdP subject</th>
                  <th className="hidden px-2 py-2 font-medium md:table-cell">
                    First seen
                  </th>
                  <th className="px-2 py-2 text-right font-medium">Status</th>
                </tr>
              </thead>
              <tbody className="divide-y">
                {users.map((user) => (
                  <tr key={user.id}>
                    <td className="px-2 py-2">
                      <Link
                        to={`/settings/users/${user.id}`}
                        className="hover:underline"
                      >
                        <div className="font-medium">{user.name}</div>
                        <div className="text-xs text-muted-foreground">
                          {user.email}
                        </div>
                      </Link>
                    </td>
                    <td className="px-2 py-2 font-mono text-xs text-muted-foreground">
                      {user.oidc_subject ?? '—'}
                    </td>
                    <td className="hidden px-2 py-2 text-xs text-muted-foreground md:table-cell">
                      {formatDate(user.created_at)}
                    </td>
                    <td className="px-2 py-2 text-right">
                      <div className="inline-flex items-center gap-1">
                        {user.mfa_enabled && (
                          <Badge variant="outline" className="gap-1">
                            <ShieldCheck className="h-3 w-3" />
                            MFA
                          </Badge>
                        )}
                        {!user.email_verified && (
                          <Badge variant="secondary">Unverified</Badge>
                        )}
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
