// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  addServicePlugin,
  approvePluginUpdate,
  disablePlugin,
  enablePlugin,
  getPluginServices,
  listExternalPlugins,
  reloadPlugins,
  uninstallPlugin,
} from '@/api/client/sdk.gen'
import type { AddServicePlugin, ReloadResponse } from '@/api/client/types.gen'
import type { PluginManifest } from '@/types/plugins'
import {
  queryOptions,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'

export const PLUGINS_QUERY_KEY = ['external-plugins']
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
    // Plugins are also added from the CLI and follow their service's
    // deployments. Refresh the list when the user returns to the console.
    refetchOnWindowFocus: 'always',
    retry: false,
  })
}

export function usePlugins() {
  return useQuery(pluginManifestQueryOptions())
}

/**
 * Remove a plugin: revoke its permissions and stop routing to it. The service
 * it runs as, and the files it stored, are kept.
 */
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

/**
 * Approve the update of a plugin that waits. `review` names the build and
 * manifest that were shown; anything else is refused. Nothing the update
 * newly asks for is allowed by approving it.
 */
export function useApprovePluginUpdate() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async ({ name, review }: { name: string; review: string }) =>
      (
        await approvePluginUpdate({
          path: { name },
          body: { review },
          throwOnError: true,
        })
      ).data,
    retry: false,
    // Approved or refused, what waits may have changed.
    onSettled: async () => {
      await queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
}

/**
 * Cut a plugin off from Temps at once. Its service, its permissions and its
 * approved build are kept for when it is enabled.
 */
export function useDisablePlugin() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (name: string) =>
      (await disablePlugin({ path: { name }, throwOnError: true })).data,
    retry: false,
    // Also when it failed: the plugin may be cut off without that being saved.
    onSettled: async () => {
      await queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
}

/** Let a disabled plugin connect again, with the permissions it had. */
export function useEnablePlugin() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (name: string) =>
      (await enablePlugin({ path: { name }, throwOnError: true })).data,
    retry: false,
    onSettled: async () => {
      await queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
}

/** Connect again to every plugin's service. */
export function useReloadPlugins() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async (): Promise<ReloadResponse> => {
      const response = await reloadPlugins({ throwOnError: true })
      return response.data
    },
    // Every plugin is connected again, so the list, its navigation and each
    // plugin's state may have changed, also when the reload failed.
    onSettled: async () => {
      await queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
}
