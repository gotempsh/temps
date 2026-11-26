// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageNamedByChromeContext, usePageNamedByChrome } from '@temps-sdk/ds'
import { Outlet, useLocation } from 'react-router'
import { PageContainer } from '@/components/layout/PageContainer'
import { PageTabStrip } from '@/components/navigation/PageTabStrip'
import { resolveSettingsTab } from './settings-navigation'

// The settings nav lives in the main app sidebar (see Sidebar.tsx —
// settings drill-down). This layout is a content-only wrapper, plus the tab
// strip of a settings entry that holds several pages (Limits).
export function SettingsLayout() {
  const { pathname } = useLocation()
  const location = resolveSettingsTab(pathname)
  // A strip above this layout (an area of the server menu that keeps a
  // `/settings` URL) names the page just as one of its own does.
  const named = usePageNamedByChrome() || Boolean(location)
  return (
    <PageContainer>
      {location?.item.tabs && (
        <PageTabStrip
          label={`${location.item.title} pages`}
          tabs={location.item.tabs.map((tab) => ({
            key: tab.url,
            title: tab.title,
            icon: tab.icon,
            to: tab.url,
            active: tab.url === location.tab.url,
          }))}
        />
      )}
      {/* A selected tab names the page; its title is not printed twice. */}
      <PageNamedByChromeContext.Provider value={named}>
        <Outlet />
      </PageNamedByChromeContext.Provider>
    </PageContainer>
  )
}
