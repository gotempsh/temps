// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Outlet } from 'react-router'

// The settings nav lives in the main app sidebar (see Sidebar.tsx —
// settings drill-down). This layout is a content-only wrapper.
export function SettingsLayout() {
  return (
    <div className="w-full px-4 sm:px-6 lg:px-8 py-6">
      <div className="max-w-7xl mx-auto min-w-0">
        <Outlet />
      </div>
    </div>
  )
}
