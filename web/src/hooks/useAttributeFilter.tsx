// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  createFacetMutation,
  listFacetsOptions,
  listFacetsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { FacetInfo, ProblemDetails } from '@/api/client/types.gen'
import {
  AttributeFilterControls,
  AttributeFilterNotice,
} from '@/components/observability/AttributeFilter'
import {
  attributeFilterApplies,
  attributesQueryValue,
  classifyAttributeKey,
  facetCreationBlocker,
  facetsInFlux,
} from '@/lib/attribute-facets'

/**
 * Wires the platform's facet list and facet creation to the attribute filter.
 *
 * `facetedOnly` is for endpoints where an unfaceted key would scan a whole
 * project: the filter is simply not sent until the key is a facet. Elsewhere an
 * unfaceted key still filters, with a notice about the cost.
 */
export function useAttributeFilter({
  attrKey,
  attrValue,
  facetedOnly,
  onKeyChange,
  onValueChange,
}: {
  attrKey: string
  attrValue: string
  facetedOnly: boolean
  onKeyChange: (key: string) => void
  onValueChange: (value: string) => void
}) {
  const queryClient = useQueryClient()
  const facetsQuery = useQuery({
    ...listFacetsOptions(),
    staleTime: 30_000,
    // A facet that is indexing or being removed changes on its own; keep the
    // notice truthful until it settles, then stop polling.
    refetchInterval: (query) =>
      facetsInFlux(query.state.data?.data) ? 3_000 : false,
  })
  // Undefined until the list arrives, so a key is never called unfaceted early.
  const facets: readonly FacetInfo[] | undefined = useMemo(
    () => facetsQuery.data?.data,
    [facetsQuery.data?.data]
  )
  const key = attrKey.trim()
  const state = classifyAttributeKey(
    facets,
    attrKey,
    attrValue,
    facetsQuery.isError
  )
  const applies = attributeFilterApplies(state, facetedOnly)

  const create = useMutation({
    ...createFacetMutation(),
    onSuccess: () => {
      toast.success(`Creating facet "${key}"`, {
        description:
          'It filters fast now; existing spans are indexed in the background.',
      })
      void queryClient.invalidateQueries({ queryKey: listFacetsQueryKey() })
    },
    onError: (error) => {
      toast.error('Failed to create facet', {
        description:
          (error as ProblemDetails)?.detail ??
          (error as ProblemDetails)?.title ??
          'Unknown error',
      })
    },
  })

  return {
    state,
    /** The trimmed key to send, or '' when the filter should not apply. */
    appliedKey: applies ? key : '',
    /** The `attributes` query value, or undefined when nothing applies. */
    query: applies ? attributesQueryValue(attrKey, attrValue) : undefined,
    controls: (
      <AttributeFilterControls
        attrKey={attrKey}
        attrValue={attrValue}
        facetKeys={(facets ?? []).map((facet) => facet.attribute_key)}
        onKeyChange={onKeyChange}
        onValueChange={onValueChange}
      />
    ),
    notice: (
      <AttributeFilterNotice
        state={state}
        attrKey={attrKey}
        facetedOnly={facetedOnly}
        hasFacets={(facets ?? []).length > 0}
        creationBlocker={facetCreationBlocker(facets ?? [], attrKey)}
        creating={create.isPending}
        onCreate={() => create.mutate({ body: { attribute_key: key } })}
        onRetry={() => void facetsQuery.refetch()}
      />
    ),
  }
}
