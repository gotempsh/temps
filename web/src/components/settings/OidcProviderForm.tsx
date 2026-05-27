import {
  applyOidcTemplate,
  getOidcTemplatePlaceholders,
  OIDC_TEMPLATE_OPTIONS,
  type OidcTemplateId,
} from '@/components/settings/oidc-provider-constants'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Separator } from '@/components/ui/separator'
import { Switch } from '@/components/ui/switch'
import { Check, Copy } from 'lucide-react'
import { useMemo, useState } from 'react'

export type OidcProviderFormValues = {
  name: string
  template: string
  enabled: boolean
  issuer_url: string
  client_id: string
  client_secret: string
  scopes: string
  default_role: string
  role_claim: string
  group_claim: string
  jit_provisioning: boolean
  trust_idp_email: boolean
}

type OidcProviderFormProps = {
  mode: 'create' | 'edit'
  value: OidcProviderFormValues
  onChange: (value: OidcProviderFormValues) => void
  redirectUri: string
  onCancel: () => void
  onSubmit: () => void
  submitting?: boolean
  submitLabel?: string
  footer?: React.ReactNode
}

function formatTemplateLabel(id: string): string {
  if (id === 'generic') return 'Generic'
  if (id === 'azure-ad') return 'Azure AD'
  return id.charAt(0).toUpperCase() + id.slice(1)
}

export function OidcProviderForm({
  mode,
  value,
  onChange,
  redirectUri,
  onCancel,
  onSubmit,
  submitting = false,
  submitLabel,
  footer,
}: OidcProviderFormProps) {
  const [copiedRedirect, setCopiedRedirect] = useState(false)

  const placeholders = useMemo(
    () => getOidcTemplatePlaceholders(value.template),
    [value.template],
  )

  const update = <K extends keyof OidcProviderFormValues>(
    key: K,
    next: OidcProviderFormValues[K],
  ) => {
    onChange({ ...value, [key]: next })
  }

  const handleTemplateChange = (template: OidcTemplateId) => {
    onChange(
      applyOidcTemplate(
        {
          ...value,
          template,
        },
        template,
      ),
    )
  }

  const copyRedirectUri = async () => {
    await navigator.clipboard.writeText(redirectUri)
    setCopiedRedirect(true)
    setTimeout(() => setCopiedRedirect(false), 2000)
  }

  return (
    <form
      className="space-y-8"
      onSubmit={(event) => {
        event.preventDefault()
        onSubmit()
      }}
    >
      <section className="space-y-4">
        <div>
          <h2 className="text-base font-semibold">Basics</h2>
        </div>
        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-2 sm:col-span-2">
            <Label htmlFor="oidc-name">Name</Label>
            <Input
              id="oidc-name"
              value={value.name}
              onChange={(event) => update('name', event.target.value)}
              placeholder={placeholders.name}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="oidc-protocol">Protocol</Label>
            <Input
              id="oidc-protocol"
              value="OpenID Connect (OIDC)"
              readOnly
              disabled
              className="bg-muted"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="oidc-template">Template</Label>
            <Select
              value={value.template}
              onValueChange={(template) =>
                handleTemplateChange(template as OidcTemplateId)
              }
            >
              <SelectTrigger id="oidc-template">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {OIDC_TEMPLATE_OPTIONS.map((option) => (
                  <SelectItem key={option.id} value={option.id}>
                    {formatTemplateLabel(option.id)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground">
              Picks sensible defaults for scopes and claim names. You can still
              override below.
            </p>
          </div>
          <div className="flex items-center justify-between rounded-lg border p-4 sm:col-span-2">
            <div className="space-y-0.5 pr-4">
              <Label htmlFor="oidc-enabled">Enabled</Label>
              <p className="text-xs text-muted-foreground">
                Disabled providers are hidden from the login screen.
              </p>
            </div>
            <Switch
              id="oidc-enabled"
              checked={value.enabled}
              onCheckedChange={(checked) => update('enabled', checked)}
            />
          </div>
        </div>
      </section>

      <Separator />

      <section className="space-y-4">
        <div>
          <h2 className="text-base font-semibold">OIDC</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            Issuer URL is required; the rest of the metadata is fetched via{' '}
            <code className="rounded bg-muted px-1 py-0.5 text-xs">
              /.well-known/openid-configuration
            </code>
            .
          </p>
        </div>
        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-2 sm:col-span-2">
            <Label htmlFor="oidc-issuer">Issuer URL</Label>
            <Input
              id="oidc-issuer"
              value={value.issuer_url}
              onChange={(event) => update('issuer_url', event.target.value)}
              placeholder={placeholders.issuer_url}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="oidc-client-id">Client ID</Label>
            <Input
              id="oidc-client-id"
              value={value.client_id}
              onChange={(event) => update('client_id', event.target.value)}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="oidc-client-secret">Client Secret</Label>
            <Input
              id="oidc-client-secret"
              type="password"
              value={value.client_secret}
              onChange={(event) => update('client_secret', event.target.value)}
              placeholder={mode === 'edit' ? 'Leave blank to keep current secret' : undefined}
              autoComplete="new-password"
            />
          </div>
          <div className="space-y-2 sm:col-span-2">
            <Label htmlFor="oidc-redirect">Redirect URL</Label>
            <div className="flex flex-col gap-2 sm:flex-row">
              <Input
                id="oidc-redirect"
                value={redirectUri}
                readOnly
                className="bg-muted font-mono text-xs"
              />
              <Button
                type="button"
                variant="outline"
                onClick={copyRedirectUri}
                className="shrink-0"
              >
                {copiedRedirect ? (
                  <Check className="mr-2 h-4 w-4" />
                ) : (
                  <Copy className="mr-2 h-4 w-4" />
                )}
                Copy
              </Button>
            </div>
            <p className="text-xs text-muted-foreground">
              Register this URL in your IdP application settings.
            </p>
          </div>
          <div className="space-y-2 sm:col-span-2">
            <Label htmlFor="oidc-scopes">Scopes (space-separated)</Label>
            <Input
              id="oidc-scopes"
              value={value.scopes}
              onChange={(event) => update('scopes', event.target.value)}
              placeholder={placeholders.scopes}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="oidc-default-role">Default Role</Label>
            <Select
              value={value.default_role}
              onValueChange={(role) => update('default_role', role)}
            >
              <SelectTrigger id="oidc-default-role">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="user">USER</SelectItem>
                <SelectItem value="admin">ADMIN</SelectItem>
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-2">
            <Label htmlFor="oidc-role-claim">Role Claim</Label>
            <Input
              id="oidc-role-claim"
              value={value.role_claim}
              onChange={(event) => update('role_claim', event.target.value)}
              placeholder={placeholders.role_claim || 'roles'}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="oidc-group-claim">Group Claim</Label>
            <Input
              id="oidc-group-claim"
              value={value.group_claim}
              onChange={(event) => update('group_claim', event.target.value)}
              placeholder={placeholders.group_claim || 'groups'}
            />
          </div>
        </div>
      </section>

      <Separator />

      <section className="space-y-4">
        <div>
          <h2 className="text-base font-semibold">First-login policy</h2>
        </div>
        <div className="flex items-center justify-between rounded-lg border p-4">
          <div className="space-y-0.5 pr-4">
            <Label htmlFor="oidc-jit">Auto-provision (default)</Label>
            <p className="text-xs text-muted-foreground">
              Create Temps users automatically on first SSO login.
            </p>
          </div>
          <Switch
            id="oidc-jit"
            checked={value.jit_provisioning}
            onCheckedChange={(checked) => update('jit_provisioning', checked)}
          />
        </div>
        <div className="flex items-center justify-between rounded-lg border border-amber-200 bg-amber-50/40 p-4 dark:border-amber-900/60 dark:bg-amber-950/20">
          <div className="space-y-1 pr-4">
            <Label htmlFor="oidc-trust-idp-email">
              Trust IdP email without{' '}
              <code className="rounded bg-muted px-1 py-0.5 text-xs">
                email_verified
              </code>{' '}
              claim
            </Label>
            <p className="text-xs text-muted-foreground">
              Enable only for corporate IdPs where an admin controls user
              provisioning — e.g. Okta Org Authorization Server, Azure AD,
              internal SSO. <strong>Do not enable</strong> for public IdPs
              that allow self-signup (Auth0 social logins, Google consumer
              accounts).
            </p>
            <p className="text-xs text-amber-700 dark:text-amber-300">
              Security risk: if an attacker can register{' '}
              <code className="rounded bg-muted px-1 py-0.5 text-xs">
                victim@example.com
              </code>{' '}
              at the IdP without verifying it, they can take over the
              victim&apos;s existing Temps account on first SSO login.
            </p>
          </div>
          <Switch
            id="oidc-trust-idp-email"
            checked={value.trust_idp_email}
            onCheckedChange={(checked) => update('trust_idp_email', checked)}
          />
        </div>
      </section>

      {footer}

      <div className="flex justify-end gap-3 border-t pt-6">
        <Button type="button" variant="outline" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" disabled={submitting}>
          {submitting ? 'Saving...' : (submitLabel ?? 'Save')}
        </Button>
      </div>
    </form>
  )
}
