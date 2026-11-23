// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getModelCatalog } from '@/api/client'
import {
  CatalogModelsTable,
  UnpricedModelsAlert,
} from '@/components/ai/models/CatalogModelsTable'
import { CodingAgentsCard } from '@/components/ai/models/CodingAgentsCard'
import {
  ModelCatalogCard,
  ModelCatalogCardSkeleton,
} from '@/components/ai/models/ModelCatalogCard'
import { MODEL_CATALOG_QUERY_KEY } from '@/components/ai/models/query-keys'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { ReadFailure } from '@/components/ui/read-failure'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'

/**
 * Models newer than this release (ADR 112): the runtime catalog that prices
 * them, the models no price covers, and the coding-agent CLIs that offer them.
 */
export function AiGatewayModelsPage() {
  usePageTitle('AI models')
  const status = useQuery({
    queryKey: [...MODEL_CATALOG_QUERY_KEY, 'status'],
    queryFn: async () => {
      const { data } = await getModelCatalog({ throwOnError: true })
      return data
    },
    // Show a refresh that is running finish without a reload.
    refetchInterval: (query) => (query.state.data?.refreshing ? 3000 : false),
  })

  return (
    <PageContainer>
      <PageHeader title="AI models" />
      <div className="space-y-6">
        <UnpricedModelsAlert />
        {status.isError && !status.data ? (
          <ReadFailure
            resource="the model catalog"
            error={status.error}
            onRetry={() => status.refetch()}
            retrying={status.isFetching}
          />
        ) : status.data ? (
          <ModelCatalogCard status={status.data} />
        ) : (
          <ModelCatalogCardSkeleton />
        )}
        <CodingAgentsCard />
        <CatalogModelsTable />
      </div>
    </PageContainer>
  )
}
