// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  DropOffPoint,
  PageFlowEntry,
  PageTransition,
  ProjectResponse,
} from '@/api/client/types.gen'

export interface PageFlowProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function durationSortValue(seconds: number | null | undefined) {
  return seconds != null && seconds > 0 ? seconds : null
}

export type EntryPageSortKey = 'page' | 'entries' | 'views' | 'bounce' | 'time'
export type ExitPageSortKey = 'page' | 'exits' | 'views' | 'rate' | 'time'
export type DropOffSortKey = 'page' | 'exits' | 'views' | 'rate'
export type TransitionSortKey = 'from' | 'to' | 'count' | 'percentage'

/**
 * Maps a column key to the value its header sorts by. Keeping the mapping
 * beside the column keys means a header that is wired to the wrong field --
 * which looks exactly like "the header toggles but the rows never move" --
 * is caught by a unit test instead of in the browser.
 */
export function entryPageSortValue(
  page: PageFlowEntry,
  key: EntryPageSortKey
): string | number | null {
  switch (key) {
    case 'page':
      return page.page_path
    case 'entries':
      return page.entry_count
    case 'views':
      return page.total_views
    case 'bounce':
      return page.bounce_rate
    case 'time':
      return durationSortValue(page.avg_time_on_page)
  }
}

export function exitPageSortValue(
  page: PageFlowEntry,
  key: ExitPageSortKey
): string | number | null {
  switch (key) {
    case 'page':
      return page.page_path
    case 'exits':
      return page.exit_count
    case 'views':
      return page.total_views
    case 'rate':
      return page.exit_rate
    case 'time':
      return durationSortValue(page.avg_time_on_page)
  }
}

export function dropOffSortValue(
  point: DropOffPoint,
  key: DropOffSortKey
): string | number {
  switch (key) {
    case 'page':
      return point.page_path
    case 'exits':
      return point.exit_count
    case 'views':
      return point.total_views
    case 'rate':
      return point.exit_rate
  }
}

export function transitionSortValue(
  transition: PageTransition,
  key: TransitionSortKey
): string | number {
  switch (key) {
    case 'from':
      return transition.from_page
    case 'to':
      return transition.to_page
    case 'count':
      return transition.transition_count
    case 'percentage':
      return transition.percentage
  }
}
