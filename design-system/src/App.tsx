// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { NavLink, Navigate, Route, Routes } from 'react-router'
import { cn } from '@temps-sdk/ds'
import Guide from './pages/Guide'
import Components from './pages/Components'
import DeploymentsLedger from './pages/DeploymentsLedger'
import DeploymentDetail from './pages/DeploymentDetail'
import ProjectSettings from './pages/ProjectSettings'

const NAV = [
  { to: '/guide', label: 'Guide' },
  { to: '/components', label: 'Components' },
  { to: '/ledger', label: 'Ledger — Deployments' },
  { to: '/detail', label: 'Detail — Deployment' },
  { to: '/settings', label: 'Settings — Project' },
]

export default function App() {
  return (
    <div className="min-h-screen bg-background text-foreground">
      <header className="border-b">
        <div className="mx-auto flex max-w-6xl items-center gap-1 overflow-x-auto px-4 py-3">
          <span className="mr-4 shrink-0 text-sm font-semibold">@temps-sdk/ds</span>
          {NAV.map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              className={({ isActive }) =>
                cn(
                  'shrink-0 rounded-md px-3 py-1.5 text-sm',
                  isActive ? 'bg-secondary font-medium' : 'text-muted-foreground hover:bg-accent',
                )
              }
            >
              {item.label}
            </NavLink>
          ))}
        </div>
      </header>
      <main>
        <Routes>
          <Route path="/" element={<Navigate to="/guide" replace />} />
          <Route path="/guide" element={<Guide />} />
          <Route path="/components" element={<Components />} />
          <Route path="/ledger" element={<DeploymentsLedger />} />
          <Route path="/detail" element={<DeploymentDetail />} />
          <Route path="/settings" element={<ProjectSettings />} />
        </Routes>
      </main>
    </div>
  )
}
