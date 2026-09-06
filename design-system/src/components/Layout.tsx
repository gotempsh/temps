// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { useTheme } from 'next-themes'
import { NavLink } from 'react-router'
import {
  Blocks,
  BookOpen,
  Fingerprint,
  Layers,
  LayoutTemplate,
  Megaphone,
  Menu,
  Moon,
  Palette,
  Layers2,
  Sun,
  Terminal,
  X,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { LogoMark } from '@/components/Logo'
import { cn } from '@/lib/utils'

const NAV = [
  { to: '/guide', label: 'Guide', icon: BookOpen },
  { to: '/brand', label: 'Brand', icon: Fingerprint },
  { to: '/foundations', label: 'Foundations', icon: Palette },
  { to: '/components', label: 'Components', icon: Blocks },
  { to: '/op-components', label: 'Operator components', icon: Blocks },
  { to: '/patterns', label: 'Page patterns', icon: LayoutTemplate },
  { to: '/kitchen-sink', label: 'Kitchen sink', icon: Layers },
  { to: '/v1', label: 'Operator console v1', icon: Layers2 },
  { to: '/v1-landing', label: 'Landing (v1)', icon: Megaphone },
  { to: '/status-page', label: 'Status page (public)', icon: Megaphone },
  { to: '/agent', label: 'Agent conversation', icon: Terminal },
]

function ThemeToggle() {
  const { resolvedTheme, setTheme } = useTheme()
  const isDark = resolvedTheme === 'dark'
  return (
    <Button
      variant="outline"
      size="icon"
      onClick={() => setTheme(isDark ? 'light' : 'dark')}
    >
      {isDark ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
      <span className="sr-only">Toggle theme</span>
    </Button>
  )
}

function NavList({ onNavigate }: { onNavigate?: () => void }) {
  return (
    <nav className="flex flex-col gap-1">
      {NAV.map(({ to, label, icon: Icon }) => (
        <NavLink
          key={to}
          to={to}
          onClick={onNavigate}
          className={({ isActive }) =>
            cn(
              'flex items-center gap-2 rounded-md px-3 py-2 text-sm font-medium transition-colors',
              isActive
                ? 'bg-sidebar-accent text-sidebar-accent-foreground'
                : 'text-sidebar-foreground/80 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground'
            )
          }
        >
          <Icon className="h-4 w-4" />
          {label}
        </NavLink>
      ))}
    </nav>
  )
}

export function Layout({ children }: { children: ReactNode }) {
  const [mobileOpen, setMobileOpen] = useState(false)

  return (
    <div className="flex min-h-screen">
      {/* Desktop sidebar — mirrors DESIGN.md §6.12 flat-list nav pattern */}
      <aside className="hidden w-60 shrink-0 border-r border-sidebar-border bg-sidebar lg:block">
        <div className="flex h-full flex-col gap-6 p-4">
          <div className="flex items-center gap-2 px-1">
            <LogoMark size={22} />
            <div className="space-y-0.5">
              <p className="text-sm font-semibold tracking-tight text-sidebar-foreground">
                Temps design system
              </p>
              <p className="text-xs text-sidebar-foreground/60">
                Mirrors temps/DESIGN.md
              </p>
            </div>
          </div>
          <NavList />
        </div>
      </aside>

      {/* Mobile nav */}
      {mobileOpen && (
        <div className="fixed inset-0 z-50 flex lg:hidden">
          <div
            className="fixed inset-0 bg-background/80 backdrop-blur-sm"
            onClick={() => setMobileOpen(false)}
          />
          <aside className="relative flex w-64 flex-col gap-6 border-r border-sidebar-border bg-sidebar p-4">
            <div className="flex items-center justify-between">
              <p className="text-sm font-semibold text-sidebar-foreground">
                Temps design system
              </p>
              <Button
                variant="ghost"
                size="icon"
                onClick={() => setMobileOpen(false)}
              >
                <X className="h-4 w-4" />
              </Button>
            </div>
            <NavList onNavigate={() => setMobileOpen(false)} />
          </aside>
        </div>
      )}

      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 items-center justify-between gap-2 border-b px-4 sm:px-6">
          <Button
            variant="outline"
            size="icon"
            className="lg:hidden"
            onClick={() => setMobileOpen(true)}
          >
            <Menu className="h-4 w-4" />
          </Button>
          <p className="hidden text-xs text-muted-foreground lg:block">
            Standalone reference — not wired into the live console. Uses the
            real primitives from <code className="font-mono">temps/web/src/components/ui</code>.
          </p>
          <ThemeToggle />
        </header>
        <main className="w-full flex-1 p-4 sm:p-6 lg:p-8">{children}</main>
      </div>
    </div>
  )
}
