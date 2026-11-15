// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { AuthFlavorDto, ProviderCatalogDto } from '@/api/client'
import { problemDetail } from '@/lib/api-problem'

export const LOCAL_LOGIN_METHOD = 'local'
/** One-click subscription sign-in run by the server's own CLI. */
export const CONSOLE_LOGIN_METHOD = 'console-login'

/** Credential format of an OpenAI-compatible endpoint (OpenCode only). */
export const OPENAI_COMPATIBLE_FORMAT = 'openai_compatible'
/** OpenCode addresses the endpoint's models as `openai-compatible/<model>`. */
const OPENAI_COMPATIBLE_MODEL_PREFIX = 'openai-compatible/'

/** The JSON document the server stores, encrypted, for an endpoint. */
export function openAiCompatibleCredential(baseUrl: string, apiKey: string) {
  return JSON.stringify({ base_url: baseUrl.trim(), api_key: apiKey.trim() })
}

/** OpenCode model selection for a model id the endpoint serves. */
export function openAiCompatibleModelSelection(model: string) {
  return `${OPENAI_COMPATIBLE_MODEL_PREFIX}${model.trim()}`
}

/** The endpoint's own model id inside an OpenCode selection, if it is one. */
export function openAiCompatibleUpstreamModel(selection?: string | null) {
  return selection?.startsWith(OPENAI_COMPATIBLE_MODEL_PREFIX)
    ? selection.slice(OPENAI_COMPATIBLE_MODEL_PREFIX.length)
    : ''
}

/** One way to connect a harness: paste a credential, or import a host login. */
export type HarnessConnectionMethod =
  | { kind: 'login'; id: typeof CONSOLE_LOGIN_METHOD; label: string }
  | { kind: 'local'; id: typeof LOCAL_LOGIN_METHOD; label: string }
  | { kind: 'credential'; id: string; label: string; flavor: AuthFlavorDto }

/**
 * Every connection method the harness supports, in display order. A host
 * login is only offered once Temps has actually detected one — a method that
 * can only tell the user to go and create it first is noise. Claude Code never
 * offers it: the server does not accept a local Claude login.
 */
export function harnessConnectionMethods(
  provider: Pick<
    ProviderCatalogDto,
    'id' | 'auth_flavors' | 'local_credential'
  > &
    Partial<Pick<ProviderCatalogDto, 'name' | 'login_supported'>>
): HarnessConnectionMethod[] {
  const displayName = provider.name ?? provider.id
  const methods: HarnessConnectionMethod[] = provider.auth_flavors.map(
    (flavor) => ({
      kind: 'credential',
      id: flavor.id,
      // Pasting stays available as the advanced route, and for API keys.
      label:
        provider.login_supported && flavor.format !== 'api_key'
          ? 'Paste token (advanced)'
          : flavor.label,
      flavor,
    })
  )
  if (provider.local_credential && provider.id !== 'claude_cli') {
    methods.unshift({
      kind: 'local',
      id: LOCAL_LOGIN_METHOD,
      label: 'Login on this server',
    })
  }
  if (provider.login_supported) {
    // Always first and never conditional on the CLI being installed: when it
    // is not, the panel explains what is missing instead of vanishing.
    methods.unshift({
      kind: 'login',
      id: CONSOLE_LOGIN_METHOD,
      label: `Sign in with ${displayName.split(/[\s(]/)[0] || displayName}`,
    })
  }
  return methods
}

/** Preselect the saved method when replacing, otherwise the first offered. */
export function initialConnectionMethodId(
  provider: Pick<ProviderCatalogDto, 'credential_saved' | 'current_auth_type'>,
  methods: HarnessConnectionMethod[]
): string {
  const saved = provider.credential_saved
    ? methods.find((method) => method.id === provider.current_auth_type)
    : undefined
  // A saved subscription credential is replaced most simply by signing in
  // again; the pasted-token tab stays one click away.
  if (saved?.kind === 'credential' && saved.flavor.format !== 'api_key') {
    const login = methods.find((method) => method.kind === 'login')
    if (login) return login.id
  }
  return (saved ?? methods[0])?.id ?? ''
}

/** Label of the method the saved credential was connected with, if known. */
export function savedConnectionLabel(
  provider: Pick<ProviderCatalogDto, 'auth_flavors' | 'current_auth_type'>
): string | undefined {
  return provider.auth_flavors.find(
    (flavor) => flavor.id === provider.current_auth_type
  )?.label
}

export function credentialVerificationMessage(result: {
  credential_verification_status?: string
  verification_hint?: string | null
}) {
  if (result.credential_verification_status === 'verified') {
    return 'Credential verified and saved.'
  }
  return (
    result.verification_hint ||
    'Credential saved, but not verified. Choose an accessible model and verify it before continuing.'
  )
}

export function harnessCheckError(error: unknown): string {
  const fallback =
    'The environment check failed. Check the saved credential and retry.'
  return problemDetail(
    error,
    typeof error === 'string' && error.trim()
      ? error
      : error instanceof Error && error.message
        ? error.message
        : fallback
  )
}

export function harnessSetupStatus(
  provider: Pick<ProviderCatalogDto, 'credential_saved' | 'workspace_ready'>
) {
  if (!provider.credential_saved) return 'Not connected'
  return provider.workspace_ready ? 'Credential saved' : 'Needs attention'
}

/**
 * The workspace page a harness setup was opened from, if it is one: the
 * workspaces list or a single workspace. Anything else (missing, external,
 * malformed, another page) is null, and the setup pages show no way back.
 */
export function workspaceReturnTo(value: string | null): string | null {
  if (
    !value ||
    !value.startsWith('/') ||
    value.startsWith('//') ||
    /[\\\s]/.test(value)
  )
    return null
  const url = new URL(value, 'https://temps.invalid')
  if (url.origin !== 'https://temps.invalid') return null
  if (
    url.pathname !== '/workspaces' &&
    !/^\/workspaces\/[a-zA-Z0-9_-]+$/.test(url.pathname)
  )
    return null
  return `${url.pathname}${url.search}`
}

/** "Back to workspaces" for the list, "Back to workspace" for one. */
export function backToWorkspaceLabel(returnTo: string): string {
  return new URL(returnTo, 'https://temps.invalid').pathname === '/workspaces'
    ? 'Back to workspaces'
    : 'Back to workspace'
}

export function harnessSetupHref(
  providerId: string | null,
  returnTo: string | null
) {
  const path =
    '/agent-sandbox/providers' +
    (providerId ? `/${encodeURIComponent(providerId)}` : '')
  const back = workspaceReturnTo(returnTo)
  return back ? `${path}?returnTo=${encodeURIComponent(back)}` : path
}

export function harnessSectionHref(path: string, search: string): string {
  const back = workspaceReturnTo(new URLSearchParams(search).get('returnTo'))
  return back ? `${path}?${new URLSearchParams({ returnTo: back })}` : path
}
