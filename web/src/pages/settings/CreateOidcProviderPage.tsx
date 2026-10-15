// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
  OIDC_REQUIRED_FIELDS,
  oidcFormFieldErrors,
  type OidcFieldErrors,
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
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useNavigate } from 'react-router'
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
    trust_idp_email: form.trust_idp_email,
  }
}

export function CreateOidcProviderPage() {
  usePageTitle('Add SSO Provider')
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
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
      trust_idp_email: defaults.trust_idp_email ?? false,
    }
  })

  const [fieldErrors, setFieldErrors] = useState<OidcFieldErrors>({})

  // Editing a field withdraws only that field's message.
  const handleFormChange = (next: OidcProviderFormValues) => {
    setFieldErrors((current) => {
      const remaining = { ...current }
      for (const { field } of OIDC_REQUIRED_FIELDS) {
        if (next[field] !== form[field]) delete remaining[field]
      }
      return remaining
    })
    setForm(next)
  }

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
    onError: (error, variables) => {
      if (
        handleSensitiveActionError(error, () =>
          createProvider.mutate(variables)
        )
      ) {
        return
      }
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
    const errors = oidcFormFieldErrors(createFormToRequest(form))
    setFieldErrors(errors)
    const firstInvalid = OIDC_REQUIRED_FIELDS.find(({ field }) => errors[field])
    if (firstInvalid) {
      document.getElementById(firstInvalid.inputId)?.focus()
      return
    }
    createProvider.mutate({ body: createFormToRequest(form) })
  }

  return (
    <div className="w-full space-y-6 py-2">
      {verificationDialog}
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
            Connect an OpenID Connect provider, then choose how new users gain
            access. Client secrets are encrypted at rest.
          </p>
        </div>
      </div>

      <Card>
        <CardHeader className="border-b">
          <CardTitle>SSO provider form</CardTitle>
          <CardDescription>
            Start with the provider details. User access policy comes last.
          </CardDescription>
        </CardHeader>
        <CardContent className="pt-6">
          <OidcProviderForm
            mode="create"
            value={form}
            onChange={handleFormChange}
            errors={fieldErrors}
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
