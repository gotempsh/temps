// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listModelCatalogEntries, listUnpricedModels } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { ReadFailure } from '@/components/ui/read-failure'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { formatPerMillion, formatTokens } from '@/lib/ai-model-catalog'
import { useQuery } from '@tanstack/react-query'
import { TriangleAlert } from 'lucide-react'
import { useState } from 'react'
import { MODEL_CATALOG_QUERY_KEY } from './query-keys'

const PAGE_SIZE = 20
const ALL_PROVIDERS = '__all'
const PROVIDERS = [
  { value: 'anthropic', label: 'Anthropic' },
  { value: 'openai', label: 'OpenAI' },
  { value: 'gemini', label: 'Google Gemini' },
  { value: 'xai', label: 'xAI' },
  { value: 'openrouter', label: 'OpenRouter' },
]

/**
 * Enabled models no price covers: they keep working and are charged the
 * gateway's ceiling, which the operator should know about.
 */
export function UnpricedModelsAlert() {
  const unpriced = useQuery({
    queryKey: [...MODEL_CATALOG_QUERY_KEY, 'unpriced'],
    queryFn: async () => {
      const { data } = await listUnpricedModels({ throwOnError: true })
      return data
    },
  })
  // A failed check must not look like a clean one, even when an earlier
  // result is cached (a failed refetch keeps it): the operator would not know
  // that spend-cap pricing went unchecked.
  if (unpriced.isError) {
    return (
      <ReadFailure
        resource="models without a known price"
        error={unpriced.error}
        onRetry={() => unpriced.refetch()}
        retrying={unpriced.isFetching}
      />
    )
  }
  if (!unpriced.data || unpriced.data.total === 0) return null
  const shown = unpriced.data.models.slice(0, 10)
  const more = unpriced.data.total - shown.length
  return (
    <Alert>
      <TriangleAlert className="size-4" />
      <AlertTitle>
        {unpriced.data.total === 1
          ? '1 enabled model has no known price'
          : `${unpriced.data.total} enabled models have no known price`}
      </AlertTitle>
      <AlertDescription>
        <p>
          They keep working, charged the gateway’s highest rate so spend caps
          hold. Refresh the catalog once a price is published.
        </p>
        <ul className="mt-2 flex flex-wrap gap-1">
          {shown.map((model) => (
            <li key={`${model.provider_key_id}:${model.model_id}`}>
              <Badge variant="outline" className="[overflow-wrap:anywhere]">
                {model.model_id}
                <span className="ml-1 text-muted-foreground">
                  {model.provider_key_name}
                </span>
              </Badge>
            </li>
          ))}
          {more > 0 ? (
            <li className="text-xs text-muted-foreground">and {more} more</li>
          ) : null}
        </ul>
      </AlertDescription>
    </Alert>
  )
}

/** The stored catalog, searchable and paginated. */
export function CatalogModelsTable() {
  const [page, setPage] = useState(1)
  const [search, setSearch] = useState('')
  const [provider, setProvider] = useState(ALL_PROVIDERS)
  const term = search.trim()

  const entries = useQuery({
    queryKey: [...MODEL_CATALOG_QUERY_KEY, 'models', provider, term, page],
    queryFn: async () => {
      const { data } = await listModelCatalogEntries({
        query: {
          page,
          page_size: PAGE_SIZE,
          ...(term ? { search: term } : {}),
          ...(provider !== ALL_PROVIDERS ? { provider } : {}),
        },
        throwOnError: true,
      })
      return data
    },
    placeholderData: (previous) => previous,
  })

  const total = Number(entries.data?.total ?? 0)
  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE))

  return (
    <Card>
      <CardHeader>
        <CardTitle>Catalog models</CardTitle>
        <CardDescription>Prices are USD per 1M tokens.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
          <Input
            value={search}
            onChange={(event) => {
              setSearch(event.target.value)
              setPage(1)
            }}
            placeholder="Search models…"
            aria-label="Search catalog models"
            className="w-full sm:w-[280px]"
          />
          <Select
            value={provider}
            onValueChange={(value) => {
              setProvider(value)
              setPage(1)
            }}
          >
            <SelectTrigger
              className="w-full sm:w-[200px]"
              aria-label="Provider"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ALL_PROVIDERS}>All providers</SelectItem>
              {PROVIDERS.map((option) => (
                <SelectItem key={option.value} value={option.value}>
                  {option.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        {entries.isError && !entries.data ? (
          <ReadFailure
            resource="catalog models"
            error={entries.error}
            onRetry={() => entries.refetch()}
            retrying={entries.isFetching}
            embedded
          />
        ) : (
          <div className="@container">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Model</TableHead>
                  <TableHead className="text-right">Input</TableHead>
                  <TableHead className="text-right">Output</TableHead>
                  <TableHead className="hidden text-right @xl:table-cell">
                    Cache read
                  </TableHead>
                  <TableHead className="hidden text-right @2xl:table-cell">
                    Context
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {entries.isPending
                  ? Array.from({ length: 5 }, (_, index) => (
                      <TableRow key={index}>
                        <TableCell>
                          <Skeleton className="h-4 w-48" />
                        </TableCell>
                        <TableCell>
                          <Skeleton className="ml-auto h-4 w-12" />
                        </TableCell>
                        <TableCell>
                          <Skeleton className="ml-auto h-4 w-12" />
                        </TableCell>
                        <TableCell className="hidden @xl:table-cell">
                          <Skeleton className="ml-auto h-4 w-12" />
                        </TableCell>
                        <TableCell className="hidden @2xl:table-cell">
                          <Skeleton className="ml-auto h-4 w-12" />
                        </TableCell>
                      </TableRow>
                    ))
                  : null}
                {entries.data && entries.data.items.length === 0 ? (
                  <TableRow>
                    <TableCell
                      colSpan={5}
                      className="py-6 text-center text-sm text-muted-foreground"
                    >
                      {term || provider !== ALL_PROVIDERS
                        ? 'No catalog model matches.'
                        : 'No catalog stored yet. Refresh now to fetch one; until then the bundled prices apply.'}
                    </TableCell>
                  </TableRow>
                ) : null}
                {entries.data?.items.map((entry) => (
                  <TableRow key={`${entry.provider}:${entry.model_id}`}>
                    <TableCell>
                      <div className="font-medium [overflow-wrap:anywhere]">
                        {entry.display_name}
                      </div>
                      <div className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
                        {entry.provider} · {entry.model_id}
                        {entry.deprecated ? ' · deprecated' : ''}
                      </div>
                    </TableCell>
                    <TableCell className="text-right">
                      {formatPerMillion(entry.input_per_million)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatPerMillion(entry.output_per_million)}
                    </TableCell>
                    <TableCell className="hidden text-right @xl:table-cell">
                      {formatPerMillion(entry.cache_read_per_million)}
                    </TableCell>
                    <TableCell className="hidden text-right @2xl:table-cell">
                      {formatTokens(entry.context_window)}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
        {total > PAGE_SIZE ? (
          <ResponsivePagination
            page={page}
            pageSize={PAGE_SIZE}
            total={total}
            totalPages={totalPages}
            onPageChange={setPage}
            ariaLabel="Catalog models pagination"
          />
        ) : null}
      </CardContent>
    </Card>
  )
}
