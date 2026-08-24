import { client } from '@/api/client/client.gen'
import { useAuth } from '@/contexts/AuthContext'
import type { PluginManifest } from '@/types/plugins'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

export const PLUGINS_QUERY_KEY = ['external-plugins']

/**
 * Roles that carry the `system:admin` permission (see
 * `crates/temps-auth/src/permissions.rs`, `Role::Admin` and
 * `Role::PlatformAdmin`) — the same roles `GET /x/plugins/registry` and
 * `POST /x/plugins/install` are `permission_guard!`-ed to on the backend.
 */
const PLUGIN_MANAGE_ROLES = ['admin', 'platform_admin']

/**
 * Whether the current user may install/manage plugins. Used to hide or
 * disable install controls for non-admins, so the UI doesn't offer an
 * action that will always 403 — the availability/install endpoints are
 * admin-gated on the backend, unlike the read-only status endpoint.
 */
export function useCanManagePlugins(): boolean {
  const { user } = useAuth()
  return !!user && PLUGIN_MANAGE_ROLES.includes(user.role)
}

/**
 * Fetch the list of external plugin manifests from /api/x/plugins.
 * Returns an empty array if the endpoint is unavailable (e.g., no plugins loaded).
 */
async function fetchPluginManifests(): Promise<PluginManifest[]> {
  try {
    const response = await client.get<PluginManifest[]>({
      url: '/x/plugins',
    })
    return response.data ?? []
  } catch {
    // Endpoint may not exist if no external plugins are configured.
    // Degrade gracefully — no plugins is the default.
    return []
  }
}

/** Response from POST /x/plugins/reload */
export interface ReloadPluginsResponse {
  loaded: number
  plugins: string[]
  message: string
}

/**
 * React Query hook to get the list of external plugins.
 * Caches for 5 minutes since plugins rarely change at runtime.
 * Never throws — returns an empty list on failure.
 */
export function usePlugins() {
  return useQuery({
    queryKey: PLUGINS_QUERY_KEY,
    queryFn: fetchPluginManifests,
    staleTime: 5 * 60 * 1000,
    gcTime: 10 * 60 * 1000,
    retry: false,
  })
}

/**
 * Mutation hook to reload all external plugins.
 * On success, invalidates the plugins query so the UI refreshes.
 */
export function useReloadPlugins() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async (): Promise<ReloadPluginsResponse> => {
      const response = await client.post<ReloadPluginsResponse>({
        url: '/x/plugins/reload',
      })
      return response.data!
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
}

// ---------------------------------------------------------------------------
// Plugin marketplace (install-from-registry) — mirrors the handlers in
// crates/temps-external-plugins/src/handler.rs. These three endpoints are
// not in the generated OpenAPI client because that client is regenerated
// from the live server and hasn't been refreshed for this feature yet, so
// calls go through the same raw `client.get/post` escape hatch used above
// for `/x/plugins` and `/x/plugins/reload`. Swap for the generated SDK
// functions once `bun run openapi-ts` picks these up.

/** A single platform's download descriptor inside a registry manifest. */
export interface PluginPlatformAsset {
  url: string
  sha256: string
}

/** Registry manifest for an installable plugin (from the release host). */
export interface PluginRegistryManifest {
  name: string
  version: string
  platforms: Record<string, PluginPlatformAsset>
}

/** One entry from GET /x/plugins/registry — a plugin this instance knows how to install. */
export interface PluginRegistryEntry {
  /** Plugin name (registry key). */
  name: string
  /** Whether the plugin binary is already installed (present on disk). */
  installed: boolean
  /** The manifest fetched from the registry, if reachable. */
  manifest?: PluginRegistryManifest | null
  /** Human-readable reason when the manifest could not be fetched. */
  reason?: string | null
}

/** Response from GET /x/plugins/{name}/status */
export interface PluginStatusResponse {
  /** Whether the plugin is installed and its process is running. */
  configured: boolean
  /** Why the plugin is not configured (when `configured` is false). */
  reason?: string | null
  /** Console path the operator should visit to configure or install it. */
  setup_path?: string | null
}

/** Response from POST /x/plugins/install */
export interface InstallPluginResponse {
  name: string
  version: string
  path: string
  reloaded: boolean
  message: string
}

export const PLUGIN_REGISTRY_QUERY_KEY = ['external-plugins', 'registry']

export const PLUGIN_STATUS_QUERY_KEY = (name: string) => [
  'external-plugins',
  'status',
  name,
]

/**
 * React Query hook for GET /x/plugins/registry — every plugin this instance
 * knows how to install (today that's a single VibeTemps entry, but the
 * endpoint returns a list, not "the one plugin", so a second installable
 * plugin needs no frontend change).
 *
 * SystemAdmin-gated on the backend: a non-admin viewer of the Plugins page
 * gets a 403 here, which the caller should render as-is rather than retry.
 * Pass `enabled: false` for non-admin viewers so the UI doesn't fire a
 * request it already knows will 403 — see [[useCanManagePlugins]].
 */
export function usePluginRegistry(options?: { enabled?: boolean }) {
  return useQuery({
    queryKey: PLUGIN_REGISTRY_QUERY_KEY,
    queryFn: async (): Promise<PluginRegistryEntry[]> => {
      const response = await client.get<PluginRegistryEntry[], unknown, true>({
        url: '/x/plugins/registry',
        throwOnError: true,
      })
      return response.data
    },
    staleTime: 60 * 1000,
    retry: false,
    enabled: options?.enabled ?? true,
  })
}

/**
 * A single named entry from `usePluginRegistry()`. Convenience wrapper for
 * callers (like the VibeTemps install card) that only care about one plugin
 * — `data` is `undefined` both while loading and if `name` isn't a known
 * installable plugin, so callers should check `isLoading` to distinguish.
 */
export function usePluginAvailability(name: string, options?: { enabled?: boolean }) {
  const registry = usePluginRegistry(options)
  return {
    ...registry,
    data: registry.data?.find((entry) => entry.name === name),
  }
}

/**
 * React Query hook for GET /x/plugins/{name}/status.
 * Any authenticated user may call this — it is the capability-check
 * endpoint that drives the onboarding state in the UI.
 */
export function usePluginStatus(name: string) {
  return useQuery({
    queryKey: PLUGIN_STATUS_QUERY_KEY(name),
    queryFn: async (): Promise<PluginStatusResponse> => {
      const response = await client.get<PluginStatusResponse, unknown, true>({
        url: '/x/plugins/{name}/status',
        path: { name },
        throwOnError: true,
      })
      return response.data
    },
    staleTime: 30 * 1000,
    retry: false,
  })
}

/**
 * Mutation hook for POST /x/plugins/install.
 * Invalidates the availability, status, and installed-plugins queries
 * regardless of outcome — a failed install can still have partially
 * changed on-disk state (e.g. binary written, process start failed), so
 * the queries need to re-read the server's view either way.
 */
export function useInstallPlugin(name: string) {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async (version?: string): Promise<InstallPluginResponse> => {
      const response = await client.post<InstallPluginResponse, unknown, true>({
        url: '/x/plugins/install',
        body: { name, version },
        throwOnError: true,
      })
      return response.data
    },
    onSettled: () => {
      queryClient.invalidateQueries({ queryKey: PLUGIN_REGISTRY_QUERY_KEY })
      queryClient.invalidateQueries({ queryKey: PLUGIN_STATUS_QUERY_KEY(name) })
      queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
}

// ---------------------------------------------------------------------------
// Published catalogue (browse what exists) — GET /x/plugins/catalog.
//
// Distinct from `usePluginRegistry` above: the registry reports on plugins
// *this build* knows how to install, so a plugin published after this binary
// was cut can never appear there. The catalogue is fetched from the release
// registry, which is the only way an operator can learn a newer plugin
// exists — and the only way they can be told that upgrading is what unlocks
// it. Every entry is verified server-side against the compile-time
// allowlist before it reaches here; see crates/temps-external-plugins/src/catalog.rs.

/** Why a catalogued plugin cannot be installed by this build. */
export type CatalogRejection = 'unknown_to_this_release' | 'manifest_url_mismatch'

/** One locally verified entry from GET /x/plugins/catalog. */
export interface PluginCatalogEntry {
  name: string
  title: string
  summary: string
  description: string
  author: string
  category: string
  repository?: string | null
  docs_url?: string | null
  latest_version?: string | null
  platforms: string[]
  /** Binary already present on this host. */
  installed: boolean
  /** Whether this build would accept an install request for it. */
  installable: boolean
  rejection?: CatalogRejection | null
  reason?: string | null
}

/** Response from GET /x/plugins/catalog. */
export interface PluginCatalogResponse {
  /** False when the registry was unreachable or unparsable. */
  available: boolean
  reason?: string | null
  /** Registry endpoint consulted, so a failure names the host that failed. */
  source: string
  plugins: PluginCatalogEntry[]
}

export const PLUGIN_CATALOG_QUERY_KEY = ['external-plugins', 'catalog']

/**
 * React Query hook for GET /x/plugins/catalog.
 *
 * SystemAdmin-gated on the backend, so pass `enabled: false` for non-admin
 * viewers rather than firing a request that is known to 403 — see
 * [[useCanManagePlugins]].
 *
 * A registry outage is a successful response with `available: false`, not an
 * error: "the catalogue is unreachable" is a state the page must render, and
 * conflating it with a request failure would leave an air-gapped operator
 * looking at a generic error with no explanation.
 */
export function usePluginCatalog(options?: { enabled?: boolean }) {
  return useQuery({
    queryKey: PLUGIN_CATALOG_QUERY_KEY,
    queryFn: async (): Promise<PluginCatalogResponse> => {
      const response = await client.get<PluginCatalogResponse, unknown, true>({
        url: '/x/plugins/catalog',
        throwOnError: true,
      })
      return response.data
    },
    staleTime: 5 * 60 * 1000,
    retry: false,
    enabled: options?.enabled ?? true,
  })
}
