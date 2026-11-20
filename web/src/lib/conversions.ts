// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Conversions (ADR 109 D2): the console's name for `analytics_goals`.
 *
 * A conversion is an event (by name, optionally narrowed by one custom
 * property) or a page view (by path, `*` as a wildcard). The form below edits
 * only those fields; any other filter a goal was given through the API is
 * carried over untouched when it is saved.
 */

import type {
  AnalyticsGoal,
  AnalyticsGoalBreakdownDimension,
  AnalyticsGoalInput,
  SmartFilter,
} from '@/api/client/types.gen'
import { z } from 'zod'

const PROPERTY_KEY = /^[A-Za-z0-9_]+(\.[A-Za-z0-9_]+)*$/
const REVENUE_KEY = /^[A-Za-z0-9_.-]{1,64}$/

export const conversionFormSchema = z
  .object({
    name: z.string().trim().min(1, 'Give the conversion a name').max(100),
    kind: z.enum(['event', 'page']),
    eventName: z.string().trim().max(255),
    pagePath: z.string().trim().max(512),
    propertyKey: z.string().trim(),
    propertyValue: z.string().trim(),
    revenueProperty: z.string().trim(),
    revenueCurrency: z.string().trim(),
  })
  .superRefine((values, ctx) => {
    if (values.kind === 'event' && !values.eventName) {
      ctx.addIssue({
        code: 'custom',
        path: ['eventName'],
        message: 'Name the event that counts as a conversion',
      })
    }
    if (values.kind === 'page' && !values.pagePath.startsWith('/')) {
      ctx.addIssue({
        code: 'custom',
        path: ['pagePath'],
        message: 'A page path starts with /, for example /thank-you',
      })
    }
    if (values.kind === 'event' && values.propertyKey) {
      if (!PROPERTY_KEY.test(values.propertyKey)) {
        ctx.addIssue({
          code: 'custom',
          path: ['propertyKey'],
          message: 'Use letters, digits and _; separate nested keys with .',
        })
      }
      if (!values.propertyValue) {
        ctx.addIssue({
          code: 'custom',
          path: ['propertyValue'],
          message: 'Say which value the property must have',
        })
      }
    }
    const wantsRevenue = Boolean(
      values.revenueProperty || values.revenueCurrency
    )
    if (values.kind === 'event' && wantsRevenue) {
      if (!REVENUE_KEY.test(values.revenueProperty)) {
        ctx.addIssue({
          code: 'custom',
          path: ['revenueProperty'],
          message: 'Name the event property that holds the amount',
        })
      }
      if (!/^[A-Za-z]{3}$/.test(values.revenueCurrency)) {
        ctx.addIssue({
          code: 'custom',
          path: ['revenueCurrency'],
          message: 'Use a three-letter currency code, for example EUR',
        })
      }
    }
  })

export type ConversionFormValues = z.infer<typeof conversionFormSchema>

export const EMPTY_CONVERSION_FORM: ConversionFormValues = {
  name: '',
  kind: 'event',
  eventName: '',
  pagePath: '',
  propertyKey: '',
  propertyValue: '',
  revenueProperty: '',
  revenueCurrency: '',
}

type CustomDataFilter = Extract<SmartFilter, { type: 'custom_data' }>

function isCustomData(filter: SmartFilter): filter is CustomDataFilter {
  return filter.type === 'custom_data'
}

/** Form values for an existing goal; the first custom property is editable. */
export function goalToFormValues(goal: AnalyticsGoal): ConversionFormValues {
  const property = goal.event_filter.find(isCustomData)
  return {
    name: goal.name,
    kind: goal.kind,
    eventName: goal.event_name ?? '',
    pagePath: goal.page_path ?? '',
    propertyKey: property?.value.path ?? '',
    propertyValue: property?.value.value ?? '',
    revenueProperty: goal.revenue_property ?? '',
    revenueCurrency: goal.revenue_currency ?? '',
  }
}

/**
 * Request body for the form. `previous` is the goal being edited: filters the
 * form does not show are kept as they were.
 */
export function formValuesToInput(
  values: ConversionFormValues,
  previous?: AnalyticsGoal
): AnalyticsGoalInput {
  const kept = previous?.event_filter ?? []
  const firstCustom = kept.findIndex(isCustomData)
  const untouched = kept.filter((_, index) => index !== firstCustom)
  const isEvent = values.kind === 'event'
  const property: SmartFilter[] =
    isEvent && values.propertyKey
      ? [
          {
            type: 'custom_data',
            value: { path: values.propertyKey, value: values.propertyValue },
          },
        ]
      : []
  const revenue = isEvent && values.revenueProperty !== ''
  return {
    name: values.name,
    kind: values.kind,
    event_name: isEvent ? values.eventName : null,
    page_path: isEvent ? null : values.pagePath,
    event_filter: [...untouched, ...property],
    revenue_property: revenue ? values.revenueProperty : null,
    revenue_currency: revenue ? values.revenueCurrency.toUpperCase() : null,
  }
}

/** One line describing what a goal matches. */
export function describeGoal(goal: AnalyticsGoal): string {
  if (goal.kind === 'page') return `Visit to ${goal.page_path ?? ''}`
  const property = goal.event_filter.find(isCustomData)
  const base = `Event ${goal.event_name ?? ''}`
  return property
    ? `${base} where ${property.value.path} = ${property.value.value}`
    : base
}

export function formatRate(rate: number): string {
  return `${rate.toFixed(rate > 0 && rate < 10 ? 1 : 0)}%`
}

export function formatRevenue(amount: number, currency: string): string {
  try {
    return new Intl.NumberFormat('en-US', {
      style: 'currency',
      currency,
      maximumFractionDigits: 2,
    }).format(amount)
  } catch {
    return `${amount.toFixed(2)} ${currency}`
  }
}

export const BREAKDOWN_DIMENSIONS: Array<{
  value: AnalyticsGoalBreakdownDimension
  label: string
}> = [
  { value: 'channel', label: 'Channel' },
  { value: 'referrer', label: 'Referrer' },
  { value: 'utm_source', label: 'UTM source' },
  { value: 'utm_medium', label: 'UTM medium' },
  { value: 'utm_campaign', label: 'UTM campaign' },
  { value: 'page', label: 'Page' },
  { value: 'country', label: 'Country' },
  { value: 'device_type', label: 'Device' },
  { value: 'browser', label: 'Browser' },
  { value: 'operating_system', label: 'Operating system' },
]
