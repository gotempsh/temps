// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Plus } from 'lucide-react'
import { Link } from 'react-router'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { PageTabStrip, type PageTab } from './PageTabStrip'

/** One view of an entity: its deployments, its logs, its settings. */
export interface EntityView extends PageTab {
  /**
   * False once the view is known to hold nothing (no trace received, no
   * domain added). It is then offered under "Add" instead of taking a tab.
   */
  inUse: boolean
  /** What is missing, said beside the view under "Add". */
  missing?: string
}

/** The views that hold nothing yet, as the entries of an "Add" menu. */
export function UnusedViewItems({
  views,
  onNavigate,
}: {
  views: EntityView[]
  onNavigate?: () => void
}) {
  return (
    <>
      <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
        Not set up yet
      </DropdownMenuLabel>
      {views.map((view) => (
        <DropdownMenuItem key={view.key} asChild>
          <Link
            to={view.to}
            onClick={onNavigate}
            className="items-start gap-2.5"
          >
            <view.icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
            <span className="min-w-0">
              <span className="block font-medium">{view.title}</span>
              {view.missing && (
                <span className="block text-xs text-muted-foreground">
                  {view.missing}
                </span>
              )}
            </span>
          </Link>
        </DropdownMenuItem>
      ))}
    </>
  )
}

/**
 * An entity's views as the one row of tabs of its page: the things it is
 * (deployments, logs, errors), then the data it has. A view with several
 * pages opens a menu of them. A view that holds nothing yet is not a tab;
 * it is listed under "Add", so what is not set up stays one click away
 * without a tab that opens on an empty page.
 */
export function EntityViewTabs({
  label,
  views,
  showAll = false,
  className,
}: {
  /** Names the strip for assistive technology, such as "web views". */
  label: string
  views: EntityView[]
  /** Every view as a tab, whatever it holds (the quick tour points at them). */
  showAll?: boolean
  className?: string
}) {
  const listed = (view: EntityView) => showAll || view.inUse || view.active
  const unused = views.filter((view) => !listed(view))
  return (
    <PageTabStrip
      label={label}
      className={className}
      tabs={views.filter(listed)}
      trailing={
        unused.length > 0 && (
          <DropdownMenu>
            <DropdownMenuTrigger className="inline-flex min-h-10 shrink-0 items-center gap-1.5 whitespace-nowrap border-b-2 border-transparent py-3 text-sm font-medium hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring data-[state=open]:text-foreground">
              <Plus className="size-4" aria-hidden="true" />
              Add
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="w-72">
              <UnusedViewItems views={unused} />
            </DropdownMenuContent>
          </DropdownMenu>
        )
      }
    />
  )
}
