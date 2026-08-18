import { useQuery } from '@tanstack/react-query'
import { getFeatureMaturityOptions } from '@/api/client/@tanstack/react-query.gen'

export function useFeatureMaturity(featureKey?: string) {
  const query = useQuery({
    ...getFeatureMaturityOptions(),
    staleTime: Infinity,
    retry: false,
  })

  return {
    ...query,
    feature: featureKey
      ? query.data?.find((entry) => entry.key === featureKey)
      : undefined,
  }
}
