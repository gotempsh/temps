import {
  deleteOidcProviderMutation,
  listOidcProvidersOptions,
  listOidcProvidersQueryKey,
  testOidcProviderMutation,
  updateOidcProviderMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  OidcProviderResponse,
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
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertCircle, KeyRound, Loader2, Plus, Trash2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { toast } from 'sonner'

export function AuthSettingsPage() {
  usePageTitle('Authentication')
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()
  const [testResult, setTestResult] = useState<OidcTestConnectionResponse | null>(
    null,
  )

  const providersQuery = useQuery(listOidcProvidersOptions())

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
      setTestResult(null)
      await queryClient.invalidateQueries({
        queryKey: listOidcProvidersQueryKey(),
      })
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
      { label: 'Authentication' },
    ])
  }, [setBreadcrumbs])

  const provider = providersQuery.data?.[0] ?? null
  const loading = providersQuery.isLoading
  const error = providersQuery.error
    ? problemMessage(providersQuery.error, 'Failed to load authentication settings')
    : null
  const redirectUri = getOidcRedirectUri()

  const handleDelete = () => {
    if (!provider) return
    deleteProvider.mutate({ path: { provider_id: provider.id } })
  }

  const handleTest = () => {
    if (!provider) return
    setTestResult(null)
    testProvider.mutate({ path: { provider_id: provider.id } })
  }

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin" />
        Loading authentication settings...
      </div>
    )
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Authentication</h1>
          <p className="text-sm text-muted-foreground">
            OIDC provider configuration. The client secret is encrypted at rest.
            Test the connection before enabling.
          </p>
        </div>
        {!provider && (
          <Button asChild>
            <Link to="/settings/auth/new">
              <Plus className="mr-2 h-4 w-4" />
              Add SSO Provider
            </Link>
          </Button>
        )}
      </div>

      {error && (
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertTitle>Could not load settings</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      {!provider ? (
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <KeyRound className="h-5 w-5" />
              SSO provider
            </CardTitle>
            <CardDescription>
              One OIDC provider per install. Password and magic-link login remain
              available alongside SSO.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <div className="rounded-lg border border-dashed p-8 text-center">
              <KeyRound className="mx-auto mb-3 h-8 w-8 text-muted-foreground" />
              <p className="text-sm font-medium">No SSO provider configured</p>
              <p className="mt-1 text-sm text-muted-foreground">
                Connect Okta, Auth0, Keycloak, Azure AD, or any OIDC-compatible IdP.
              </p>
              <Button asChild className="mt-4">
                <Link to="/settings/auth/new">Add SSO Provider</Link>
              </Button>
            </div>
          </CardContent>
        </Card>
      ) : (
        <ProviderEditor
          provider={provider}
          redirectUri={redirectUri}
          onSave={(body) =>
            updateProvider.mutate({
              path: { provider_id: provider.id },
              body,
            })
          }
          saving={updateProvider.isPending}
          onTest={handleTest}
          testing={testProvider.isPending}
          onDelete={handleDelete}
          deleting={deleteProvider.isPending}
          testResult={testResult}
        />
      )}
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
    <>
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <KeyRound className="h-5 w-5" />
            SSO provider
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
                    {testing ? 'Testing...' : 'Test connection'}
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

      <OidcRoleMappingsCard
        providerId={provider.id}
        defaultRole={provider.default_role}
      />
    </>
  )
}
