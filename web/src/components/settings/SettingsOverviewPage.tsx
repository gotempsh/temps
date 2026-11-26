// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ArrowRight } from 'lucide-react'
import { useEffect } from 'react'
import { Link } from 'react-router'
import { FeatureMaturityBadge } from '@/components/feature-maturity/FeatureMaturityBadge'
import { PageHeader } from '@/components/layout/PageContainer'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSettingsGroups } from './use-settings-groups'

/**
 * The page behind the Settings entry: every settings page by group, with one
 * line on what is set there. An index, like the other parents' overviews.
 */
export function SettingsOverviewPage() {
  const groups = useSettingsGroups()
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([{ label: 'Settings' }])
  }, [setBreadcrumbs])
  usePageTitle('Settings')
  return (
    <>
      <PageHeader title="Settings" />
      {groups.map((group) => (
        <section
          key={group.label}
          aria-labelledby={`settings-group-${group.label}`}
          className="space-y-3"
        >
          <h2
            id={`settings-group-${group.label}`}
            className="text-sm font-medium text-muted-foreground"
          >
            {group.label}
          </h2>
          <ul className="grid grid-cols-1 gap-3 md:grid-cols-2 lg:grid-cols-3">
            {group.items.map((item) => (
              <li
                key={item.id ?? item.url}
                className="group relative flex min-w-0 items-start gap-3 rounded-xl border bg-card p-4 transition-colors focus-within:border-foreground/20 hover:border-foreground/20"
              >
                <span className="grid size-9 shrink-0 place-items-center rounded-lg border bg-muted/40 text-muted-foreground [&>svg]:size-4">
                  <item.icon />
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <h3 className="truncate text-sm font-semibold">
                      {/* Stretched over the card: the whole card opens it. */}
                      <Link
                        to={item.url}
                        className="rounded-sm after:absolute after:inset-0 after:rounded-xl focus-visible:outline-none focus-visible:after:ring-2 focus-visible:after:ring-ring"
                      >
                        {item.title}
                      </Link>
                    </h3>
                    <FeatureMaturityBadge
                      featureKey={item.featureKey}
                      compact
                    />
                  </div>
                  {item.description && (
                    <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">
                      {item.description}
                    </p>
                  )}
                </div>
                <ArrowRight
                  aria-hidden="true"
                  className="size-4 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5 group-hover:text-foreground"
                />
              </li>
            ))}
          </ul>
        </section>
      ))}
    </>
  )
}
