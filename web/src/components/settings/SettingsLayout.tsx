// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Outlet } from 'react-router'
import { PageContainer } from '@/components/layout/PageContainer'

// The settings nav lives in the main app sidebar (see Sidebar.tsx —
// settings drill-down). This layout is a content-only wrapper.
export function SettingsLayout() {
  return (
    <PageContainer width="wide">
      <Outlet />
    </PageContainer>
  )
}
