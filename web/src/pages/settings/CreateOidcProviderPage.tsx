import {
  createOidcProviderMutation,
  listOidcProvidersQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import {
  OidcProviderForm,
  type OidcProviderFormValues,
} from '@/components/settings/OidcProviderForm'
import {
  createDefaultOidcProviderForm,
  getOidcRedirectUri,
  isOidcFormValid,
  problemMessage,
} from '@/components/settings/oidc-provider-constants'
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
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'

function createFormToRequest(form: OidcProviderFormValues) {
  return {
    name: form.name.trim(),
    issuer_url: form.issuer_url.trim(),
    client_id: form.client_id.trim(),
    client_secret: form.client_secret,
    scopes: form.scopes.trim(),
    enabled: form.enabled,
    jit_provisioning: form.jit_provisioning,
    template: form.template,
    group_claim: form.group_claim.trim(),
    role_claim: form.role_claim.trim(),
    default_role: form.default_role,
  }
}

export function CreateOidcProviderPage() {
  usePageTitle('Add SSO Provider')
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [form, setForm] = useState<OidcProviderFormValues>(() => {
    const defaults = createDefaultOidcProviderForm()
    return {
      name: defaults.name,
      template: defaults.template ?? 'generic',
      enabled: defaults.enabled ?? false,
      issuer_url: defaults.issuer_url,
      client_id: defaults.client_id,
      client_secret: defaults.client_secret,
      scopes: defaults.scopes ?? 'openid profile email',
      default_role: defaults.default_role ?? 'user',
      role_claim: defaults.role_claim ?? 'roles',
      group_claim: defaults.group_claim ?? 'groups',
      jit_provisioning: defaults.jit_provisioning ?? true,
    }
  })

  const redirectUri = useMemo(() => getOidcRedirectUri(), [])

  const createProvider = useMutation({
    ...createOidcProviderMutation(),
    onSuccess: async () => {
      toast.success('SSO provider saved')
      await queryClient.invalidateQueries({
        queryKey: listOidcProvidersQueryKey(),
      })
      navigate('/settings/auth')
    },
    onError: (error) => {
      toast.error(problemMessage(error, 'Failed to save SSO provider'))
    },
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Authentication', href: '/settings/auth' },
      { label: 'Add SSO provider' },
    ])
  }, [setBreadcrumbs])

  const handleSubmit = () => {
    if (!isOidcFormValid(createFormToRequest(form))) {
      toast.error('Fill in name, issuer URL, client ID, and client secret')
      return
    }
    createProvider.mutate({ body: createFormToRequest(form) })
  }

  return (
    <div className="mx-auto max-w-3xl space-y-6 py-2">
      <div className="flex items-start gap-4">
        <Button
          variant="ghost"
          size="icon"
          onClick={() => navigate('/settings/auth')}
          aria-label="Back to authentication settings"
        >
          <ArrowLeft className="h-4 w-4" />
        </Button>
        <div className="space-y-1">
          <h1 className="text-2xl font-semibold tracking-tight">
            Add SSO Provider
          </h1>
          <p className="text-sm text-muted-foreground">
            OIDC provider configuration. The client secret is encrypted at rest.
            Test the connection before enabling.
          </p>
        </div>
      </div>

      <Card>
        <CardHeader className="sr-only">
          <CardTitle>SSO provider form</CardTitle>
          <CardDescription>Configure OpenID Connect SSO</CardDescription>
        </CardHeader>
        <CardContent className="pt-6">
          <OidcProviderForm
            mode="create"
            value={form}
            onChange={setForm}
            redirectUri={redirectUri}
            onCancel={() => navigate('/settings/auth')}
            onSubmit={handleSubmit}
            submitting={createProvider.isPending}
            submitLabel="Save"
          />
        </CardContent>
      </Card>
    </div>
  )
}
