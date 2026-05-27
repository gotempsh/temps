import type { CreateOidcProviderRequest, OidcProviderResponse } from '@/api/client/types.gen'
import type { OidcProviderFormValues } from '@/components/settings/OidcProviderForm'

export type OidcTemplateDefaults = {
  scopes: string
  group_claim: string
  role_claim: string
  default_role: string
}

export const OIDC_TEMPLATE_DEFAULTS: Record<string, OidcTemplateDefaults> = {
  okta: {
    scopes: 'openid profile email groups',
    group_claim: 'groups',
    role_claim: 'roles',
    default_role: 'user',
  },
  auth0: {
    scopes: 'openid profile email',
    group_claim: 'https://temps.sh/groups',
    role_claim: 'https://temps.sh/roles',
    default_role: 'user',
  },
  keycloak: {
    scopes: 'openid profile email roles',
    group_claim: 'groups',
    role_claim: 'roles',
    default_role: 'user',
  },
  google: {
    scopes: 'openid profile email',
    group_claim: '',
    role_claim: '',
    default_role: 'user',
  },
  'azure-ad': {
    scopes: 'openid profile email',
    group_claim: 'groups',
    role_claim: 'roles',
    default_role: 'user',
  },
  generic: {
    scopes: 'openid profile email',
    group_claim: 'groups',
    role_claim: 'roles',
    default_role: 'user',
  },
}

export type OidcTemplatePlaceholders = {
  name: string
  issuer_url: string
  scopes: string
  role_claim: string
  group_claim: string
}

export const OIDC_TEMPLATE_PLACEHOLDERS: Record<string, OidcTemplatePlaceholders> = {
  okta: {
    name: 'Okta — Production',
    issuer_url: 'https://<TENANT>.okta.com/oauth2/default',
    scopes: 'openid profile email groups',
    role_claim: 'roles',
    group_claim: 'groups',
  },
  auth0: {
    name: 'Auth0 — Production',
    issuer_url: 'https://<TENANT_NAME>.<REGION>.auth0.com',
    scopes: 'openid profile email',
    role_claim: 'https://temps.sh/roles',
    group_claim: 'https://temps.sh/groups',
  },
  keycloak: {
    name: 'Keycloak — Production',
    issuer_url: 'https://<KEYCLOAK_HOST>/realms/<REALM>',
    scopes: 'openid profile email roles',
    role_claim: 'roles',
    group_claim: 'groups',
  },
  google: {
    name: 'Google Workspace',
    issuer_url: 'https://accounts.google.com',
    scopes: 'openid profile email',
    role_claim: '',
    group_claim: '',
  },
  'azure-ad': {
    name: 'Azure AD — Production',
    issuer_url: 'https://login.microsoftonline.com/<TENANT_ID>/v2.0',
    scopes: 'openid profile email',
    role_claim: 'roles',
    group_claim: 'groups',
  },
  generic: {
    name: 'My SSO Provider',
    issuer_url: 'https://idp.example.com',
    scopes: 'openid profile email',
    role_claim: 'roles',
    group_claim: 'groups',
  },
}

export function getOidcTemplatePlaceholders(
  template: string,
): OidcTemplatePlaceholders {
  return (
    OIDC_TEMPLATE_PLACEHOLDERS[template] ?? OIDC_TEMPLATE_PLACEHOLDERS.generic
  )
}

export const OIDC_TEMPLATE_OPTIONS = [
  {
    id: 'generic',
    name: 'Generic OIDC',
    description: 'Any OpenID Connect provider with standard claims',
  },
  {
    id: 'okta',
    name: 'Okta',
    description: 'Okta Workforce Identity with groups scope',
  },
  {
    id: 'auth0',
    name: 'Auth0',
    description: 'Auth0 tenant with custom role/group claims',
  },
  {
    id: 'keycloak',
    name: 'Keycloak',
    description: 'Self-hosted Keycloak realm',
  },
  {
    id: 'google',
    name: 'Google',
    description: 'Google OAuth — email/profile only, no group mapping',
  },
  {
    id: 'azure-ad',
    name: 'Azure AD',
    description: 'Microsoft Entra ID (Azure AD) OIDC',
  },
] as const

export type OidcTemplateId = (typeof OIDC_TEMPLATE_OPTIONS)[number]['id']

export function createDefaultOidcProviderForm(
  template: OidcTemplateId = 'generic',
): CreateOidcProviderRequest {
  const defaults =
    OIDC_TEMPLATE_DEFAULTS[template] ?? OIDC_TEMPLATE_DEFAULTS.generic
  return {
    name: '',
    issuer_url: '',
    client_id: '',
    client_secret: '',
    scopes: defaults.scopes,
    jit_provisioning: true,
    enabled: false,
    template,
    group_claim: defaults.group_claim,
    role_claim: defaults.role_claim,
    default_role: defaults.default_role,
    trust_idp_email: false,
  }
}

export function providerToFormValues(
  provider: OidcProviderResponse,
): OidcProviderFormValues {
  return {
    name: provider.name,
    template: provider.template,
    enabled: provider.enabled,
    issuer_url: provider.issuer_url,
    client_id: provider.client_id,
    client_secret: '',
    scopes: provider.scopes,
    default_role: provider.default_role,
    role_claim: provider.role_claim,
    group_claim: provider.group_claim,
    jit_provisioning: provider.jit_provisioning,
    trust_idp_email: provider.trust_idp_email,
  }
}

export function isOidcEditFormValid(form: OidcProviderFormValues): boolean {
  return (
    form.name.trim().length > 0 &&
    form.issuer_url.trim().length > 0 &&
    form.client_id.trim().length > 0
  )
}

export function applyOidcTemplate(
  form: OidcProviderFormValues,
  template: OidcTemplateId,
): OidcProviderFormValues {
  const defaults =
    OIDC_TEMPLATE_DEFAULTS[template] ?? OIDC_TEMPLATE_DEFAULTS.generic
  return {
    ...form,
    template,
    scopes: defaults.scopes,
    group_claim: defaults.group_claim,
    role_claim: defaults.role_claim,
    default_role: defaults.default_role,
  }
}

export function problemMessage(error: unknown, fallback: string): string {
  if (error && typeof error === 'object' && 'detail' in error) {
    const detail = (error as { detail?: unknown }).detail
    if (typeof detail === 'string' && detail.length > 0) {
      return detail
    }
  }
  if (error instanceof Error && error.message) {
    return error.message
  }
  return fallback
}

export function getOidcRedirectUri(): string {
  return `${window.location.origin}/api/auth/oidc/callback`
}

export function isOidcFormValid(form: CreateOidcProviderRequest): boolean {
  return (
    form.name.trim().length > 0 &&
    form.issuer_url.trim().length > 0 &&
    form.client_id.trim().length > 0 &&
    form.client_secret.trim().length > 0
  )
}
