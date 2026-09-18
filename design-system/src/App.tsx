// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  NavLink,
  Navigate,
  Route,
  Routes,
  useLocation,
  useNavigate,
} from 'react-router'
import { Toaster } from 'sonner'
import { cn, Field } from '@temps-sdk/ds'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import Guide from './pages/Guide'
import Iconography from './pages/Iconography'
import TableStates from './pages/TableStates'
import Components from './pages/Components'
import DeploymentsLedger from './pages/DeploymentsLedger'
import DeploymentDetail from './pages/DeploymentDetail'
import ProjectSettings from './pages/ProjectSettings'
import ReleaseNotes from './pages/ReleaseNotes'
import AiSummaries from './pages/AiSummaries'
import ProjectsGrid from './pages/ProjectsGrid'
import ConnectRepoWizard from './pages/ConnectRepoWizard'

const NAV_SECTIONS = [
  {
    label: 'Docs',
    items: [
      { to: '/guide', label: 'Guide' },
      { to: '/components', label: 'Components' },
      { to: '/iconography', label: 'Iconography' },
    ],
  },
  {
    label: 'Reference screens',
    items: [
      { to: '/ledger', label: 'Ledger — Deployments' },
      { to: '/table-states', label: 'Overview — Executions' },
      { to: '/detail', label: 'Detail — Deployment' },
      { to: '/settings', label: 'Settings — Project' },
      { to: '/card-grid', label: 'CardGrid — Projects' },
      { to: '/wizard', label: 'Wizard — Connect a repository' },
      { to: '/article', label: 'Article — Release notes' },
      { to: '/onboarding', label: 'PageState — Onboarding' },
    ],
  },
]

export default function App() {
  const { pathname } = useLocation()
  const navigate = useNavigate()
  return (
    <div className="flex min-h-screen flex-col md:flex-row bg-background text-foreground">
      <aside className="flex w-full md:w-56 shrink-0 flex-col border-b md:border-b-0 md:border-r">
        <div className="border-b px-4 py-3 text-sm font-semibold">
          @temps-sdk/ds
        </div>
        <div className="p-4 md:hidden">
          <Field label="Explore examples">
            {(props) => (
              <Select
                value={pathname === '/' ? '/guide' : pathname}
                onValueChange={(value) => navigate(value)}
              >
                <SelectTrigger {...props}>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {NAV_SECTIONS.flatMap((section) => section.items).map(
                    (item) => (
                      <SelectItem key={item.to} value={item.to}>
                        {item.label}
                      </SelectItem>
                    ),
                  )}
                </SelectContent>
              </Select>
            )}
          </Field>
        </div>
        <nav
          aria-label="Design system"
          className="hidden md:block flex-1 space-y-4 overflow-y-auto px-2 py-3"
        >
          {NAV_SECTIONS.map((section) => (
            <div key={section.label}>
              <div className="px-2 pb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
                {section.label}
              </div>
              <div className="space-y-0.5">
                {section.items.map((item) => (
                  <NavLink
                    key={item.to}
                    to={item.to}
                    className={({ isActive }) =>
                      cn(
                        'block rounded-md px-2 py-1.5 text-sm',
                        isActive
                          ? 'bg-secondary font-medium'
                          : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground',
                      )
                    }
                  >
                    {item.label}
                  </NavLink>
                ))}
              </div>
            </div>
          ))}
        </nav>
      </aside>
      <main className="min-w-0 flex-1 overflow-y-auto">
        <Routes>
          <Route path="/" element={<Navigate to="/guide" replace />} />
          <Route path="/guide" element={<Guide />} />
          <Route path="/iconography" element={<Iconography />} />
          <Route path="/components" element={<Components />} />
          <Route path="/table-states" element={<TableStates />} />
          <Route path="/ledger" element={<DeploymentsLedger />} />
          <Route path="/detail" element={<DeploymentDetail />} />
          <Route path="/settings" element={<ProjectSettings />} />
          <Route path="/card-grid" element={<ProjectsGrid />} />
          <Route path="/wizard" element={<ConnectRepoWizard />} />
          <Route path="/article" element={<ReleaseNotes />} />
          <Route path="/onboarding" element={<AiSummaries />} />
        </Routes>
      </main>
      <Toaster position="top-center" />
    </div>
  )
}
