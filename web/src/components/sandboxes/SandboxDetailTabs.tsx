// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { useLocation, useNavigate, useSearchParams } from 'react-router'
import {
  Activity,
  FolderOpen,
  Network,
  Plug,
  ShieldCheck,
  Terminal,
} from 'lucide-react'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@temps-sdk/ds'
import { PREVIEW_ACCESS_ANCHOR } from './SandboxPreviewLinksCard'

const SECTIONS = [
  { id: 'terminal', label: 'Terminal', icon: Terminal },
  { id: 'files', label: 'Files', icon: FolderOpen },
  { id: 'services', label: 'Services', icon: Plug },
  { id: 'network', label: 'Network', icon: Network },
  { id: 'access', label: 'Access', icon: ShieldCheck },
  { id: 'activity', label: 'Activity', icon: Activity },
] as const

type Section = (typeof SECTIONS)[number]['id']

/** Load panels on first use, then retain their terminal connections and drafts. */
export function SandboxDetailTabs({
  panels,
}: {
  panels: Record<Section, ReactNode>
}) {
  const location = useLocation()
  const navigate = useNavigate()
  const [search] = useSearchParams()
  const requested =
    location.hash === `#${PREVIEW_ACCESS_ANCHOR}` ? 'access' : search.get('tab')
  const active =
    SECTIONS.find((section) => section.id === requested)?.id ?? 'terminal'
  const requestedTab = useRef(active)
  useLayoutEffect(() => {
    requestedTab.current = active
  }, [active, location.key])
  const [visited, setVisited] = useState<Set<Section>>(() => new Set([active]))
  if (!visited.has(active)) setVisited(new Set([...visited, active]))

  return (
    <Tabs
      value={active}
      onValueChange={(value) => {
        // Mouse-down and focus can both activate a tab before navigation renders.
        if (requestedTab.current === value) return
        requestedTab.current = value as Section
        const next = new URLSearchParams(search)
        if (value === 'terminal') next.delete('tab')
        else next.set('tab', value)
        navigate({
          pathname: location.pathname,
          search: next.toString(),
          hash: '',
        })
      }}
    >
      <TabsList
        aria-label="Sandbox sections"
        className="flex-wrap gap-x-5 gap-y-0 overflow-visible"
      >
        {SECTIONS.map(({ id, label, icon }) => (
          <TabsTrigger key={id} value={id} icon={icon}>
            {label}
          </TabsTrigger>
        ))}
      </TabsList>
      {SECTIONS.map(
        ({ id }) =>
          visited.has(id) && (
            <TabsContent
              key={id}
              value={id}
              forceMount
              hidden={active !== id}
              className="space-y-6"
            >
              {panels[id]}
            </TabsContent>
          )
      )}
    </Tabs>
  )
}
