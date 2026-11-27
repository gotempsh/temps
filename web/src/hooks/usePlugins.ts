// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  addServicePlugin,
  getPluginInstallationReporting,
  getPluginSandbox,
  getPluginServices,
  listExternalPlugins,
  listPluginCatalog,
  putPluginIsolation,
  reloadPlugins,
  setPluginInstallationReporting,
  uninstallPlugin,
} from '@/api/client/sdk.gen'
import type {
  AddServicePlugin,
  PluginCatalogResponse,
  PluginIsolation,
  ReloadResponse,
} from '@/api/client/types.gen'
import type { PluginManifest } from '@/types/plugins'
import {
  queryOptions,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'

export const PLUGINS_QUERY_KEY = ['external-plugins']
export const PLUGIN_CATALOG_QUERY_KEY = ['external-plugins', 'catalog']
export const PLUGIN_REPORTING_QUERY_KEY = [
  'external-plugins',
  'installation-reporting',
]
export const PLUGIN_SANDBOX_QUERY_KEY = ['external-plugins', 'sandbox']
export const PLUGIN_SERVICES_QUERY_KEY = ['external-plugins', 'services']

/**
 * The plugins that run as Temps services, how each is doing, and whether a
 * service can be added as a plugin on this server. Administrators only: pass
 * `enabled` from the caller's permission.
 */
export function usePluginServices(enabled = true) {
  return useQuery({
    queryKey: PLUGIN_SERVICES_QUERY_KEY,
    queryFn: async () => (await getPluginServices({ throwOnError: true })).data,
    enabled,
    retry: false,
    refetchOnWindowFocus: 'always',
  })
}

/** Make a deployed service a plugin. It starts with no permissions. */
export function useAddServicePlugin() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: AddServicePlugin) =>
      (await addServicePlugin({ body, throwOnError: true })).data,
    retry: false,
    // The plugin list, its navigation and each plugin's state all change.
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
}

/**
 * Whether this server runs plugins in containers, and how each installed
 * plugin is isolated. Administrators only: pass `enabled` from the caller's
 * permission.
 */
export function usePluginSandbox(enabled = true) {
  return useQuery({
    queryKey: PLUGIN_SANDBOX_QUERY_KEY,
    queryFn: async () => (await getPluginSandbox({ throwOnError: true })).data,
    enabled,
    retry: false,
    refetchOnWindowFocus: 'always',
  })
}

/** Move an installed plugin into a container, or out of one, and restart it. */
export function useSetPluginIsolation() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async ({
      name,
      isolation,
    }: {
      name: string
      isolation: PluginIsolation
    }) =>
      (
        await putPluginIsolation({
          path: { name },
          body: { isolation },
          throwOnError: true,
        })
      ).data,
    // The plugin restarts either way: a failed move returns it to its
    // previous isolation, so the list and its navigation are refreshed too.
    onSettled: async () => {
      await queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
}

export function usePluginInstallationReporting(enabled = true) {
  return useQuery({
    queryKey: PLUGIN_REPORTING_QUERY_KEY,
    queryFn: async () =>
      (await getPluginInstallationReporting({ throwOnError: true })).data,
    enabled,
    retry: false,
  })
}

export function useSetPluginInstallationReporting() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (enabled: boolean) =>
      (
        await setPluginInstallationReporting({
          body: { enabled },
          throwOnError: true,
        })
      ).data,
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: PLUGIN_REPORTING_QUERY_KEY,
      })
    },
  })
}

/**
 * Fetch the list of running external plugin manifests.
 * The endpoint is optional, so startup without a plugin host degrades to an
 * empty list instead of breaking navigation throughout the dashboard.
 */
async function fetchPluginManifests(): Promise<PluginManifest[]> {
  try {
    const response = await listExternalPlugins({ throwOnError: true })
    return (response.data ?? []) as PluginManifest[]
  } catch {
    return []
  }
}

/**
 * React Query hook to get the list of running external plugins.
 * Caches for 5 minutes since plugins rarely change at runtime.
 */
export function pluginManifestQueryOptions() {
  return queryOptions({
    queryKey: PLUGINS_QUERY_KEY,
    queryFn: fetchPluginManifests,
    staleTime: 5 * 60 * 1000,
    gcTime: 10 * 60 * 1000,
    // CLI installs happen outside this tab. Refresh local manifests when the
    // user returns to the console, without polling the public registry.
    refetchOnWindowFocus: 'always',
    retry: false,
  })
}

export function usePlugins() {
  return useQuery(pluginManifestQueryOptions())
}

/** Fetch the signed registry catalog exposed by the backend. */
export function usePluginCatalog(enabled = true) {
  return useQuery({
    queryKey: PLUGIN_CATALOG_QUERY_KEY,
    queryFn: async (): Promise<PluginCatalogResponse> => {
      const response = await listPluginCatalog({ throwOnError: true })
      return response.data
    },
    staleTime: 5 * 60 * 1000,
    retry: false,
    enabled,
  })
}

/** Stop and uninstall the selected plugin, retaining its application data. */
export function useUninstallPlugin() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (name: string) => {
      const response = await uninstallPlugin({
        path: { name },
        throwOnError: true,
      })
      return response.data
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
}

/** Reload all verified plugin installations from disk. */
export function useReloadPlugins() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async (): Promise<ReloadResponse> => {
      const response = await reloadPlugins({ throwOnError: true })
      return response.data
    },
    // A 502 reload can stop every plugin; refresh navigation even on failure.
    onSettled: async () => {
      await queryClient.invalidateQueries({
        queryKey: PLUGINS_QUERY_KEY,
        exact: true,
      })
      // A reload restarts every plugin, so what each one runs in, and why
      // one did not start, may have changed.
      await queryClient.invalidateQueries({
        queryKey: PLUGIN_SANDBOX_QUERY_KEY,
      })
    },
  })
}
