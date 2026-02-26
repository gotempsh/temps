import { client } from '@/api/client/client.gen'
import type { PluginManifest } from '@/types/plugins'
import { useQuery } from '@tanstack/react-query'

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

/**
 * React Query hook to get the list of external plugins.
 * Caches for 5 minutes since plugins rarely change at runtime.
 * Never throws — returns an empty list on failure.
 */
export function usePlugins() {
  return useQuery({
    queryKey: ['external-plugins'],
    queryFn: fetchPluginManifests,
    staleTime: 5 * 60 * 1000,
    gcTime: 10 * 60 * 1000,
    retry: false,
  })
}
