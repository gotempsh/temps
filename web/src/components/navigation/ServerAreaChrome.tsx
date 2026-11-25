// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageNamedByChromeContext } from '@temps-sdk/ds'
import type { ReactNode } from 'react'
import { useLocation } from 'react-router'
import { PageContainer } from '@/components/layout/PageContainer'
import {
  SERVER_HOME,
  SERVER_VIEWS,
  isProjectsView,
  resolveServerLocation,
  type ServerNavArea,
  type ServerNavTab,
} from '@/lib/server-navigation'
import { PageTabStrip, type PageTab } from './PageTabStrip'

const tabsOf = (area: ServerNavArea, current: ServerNavTab): PageTab[] =>
  (area.tabs ?? []).map((item) => ({
    key: item.url,
    title: item.title,
    icon: item.icon,
    to: item.url,
    active: item.url === current.url,
    featureKey: item.featureKey,
  }))

/**
 * The tabs above a server-wide page.
 *
 * The Projects page carries what is seen across every project (logs,
 * errors, traces, analytics, alerts) as its tabs, with the pages of the one
 * shown as a second row. A branch of the tree with several pages
 * (Databases and its backups) has those as its tabs.
 *
 * Only a list page gets a strip: a record or a form below it (a database,
 * "Add domain") is one task and stands alone. The strip owns the gutters
 * here, so the page inside lines up with it and adds none of its own.
 */
export function ServerAreaChrome({ children }: { children: ReactNode }) {
  const { pathname } = useLocation()
  const { area, tab } = resolveServerLocation(pathname)
  if (!area) return <>{children}</>
  const ownPage = pathname === area.url || tab !== undefined
  if (isProjectsView(area) && ownPage) {
    const views = [SERVER_HOME, ...SERVER_VIEWS]
    return (
      <PageContainer width={tab?.width ?? 'full'} innerClassName="space-y-0">
        <PageTabStrip
          label="Projects views"
          icons={false}
          // The pages of the view shown sit right under it.
          className={tab ? 'mb-3' : undefined}
          tabs={views.map((view) => ({
            key: view.id,
            title: view.title,
            icon: view.icon,
            to: view.url,
            active: view.id === area.id,
            featureKey: view.featureKey,
          }))}
        />
        {tab && (
          <PageTabStrip
            level="secondary"
            icons={false}
            label={`${area.title} pages`}
            tabs={tabsOf(area, tab)}
          />
        )}
        {/* The selected tab names the page; its title is not printed twice. */}
        <PageNamedByChromeContext.Provider value={true}>
          {children}
        </PageNamedByChromeContext.Provider>
      </PageContainer>
    )
  }
  if (!area.tabs || !tab) return <>{children}</>
  return (
    <PageContainer width={tab.width ?? 'full'} innerClassName="space-y-0">
      <PageTabStrip label={`${area.title} pages`} tabs={tabsOf(area, tab)} />
      {/* The selected tab names the page; its title is not printed twice. */}
      <PageNamedByChromeContext.Provider value={true}>
        {children}
      </PageNamedByChromeContext.Provider>
    </PageContainer>
  )
}
