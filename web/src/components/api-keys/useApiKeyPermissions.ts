import { useQuery } from '@tanstack/react-query'
import { getApiKeyPermissions } from '@/api/client'

export function useApiKeyPermissions() {
  return useQuery({
    queryKey: ['apiKeyPermissions'],
    queryFn: async () => {
      const response = await getApiKeyPermissions()
      return response.data
    },
    staleTime: 5 * 60 * 1000, // Cache for 5 minutes
  })
}
