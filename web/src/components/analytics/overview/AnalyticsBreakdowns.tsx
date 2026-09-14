// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import {
  ChartNoAxesCombined,
  Users,
  Monitor,
  MousePointerClick,
} from 'lucide-react'
import type { ReactNode } from 'react'
import {
  Tabs,
  ScrollableTabsList,
  TabsTrigger,
  TabsContent,
} from '@/components/ui/tabs'
const icons = {
  traffic: ChartNoAxesCombined,
  audience: Users,
  technology: Monitor,
  events: MousePointerClick,
}
export function AnalyticsBreakdowns(
  props: Record<'traffic' | 'audience' | 'technology' | 'events', ReactNode>
) {
  return (
    <Tabs defaultValue="traffic" className="min-w-0">
      <ScrollableTabsList aria-label="Analytics breakdowns">
        {Object.keys(props).map((key) => (
          <TabsTrigger key={key} value={key}>
            {(() => {
              const Icon = icons[key as keyof typeof icons]
              return <Icon className="mr-1.5 h-4 w-4" aria-hidden="true" />
            })()}
            {key[0].toUpperCase() + key.slice(1)}
          </TabsTrigger>
        ))}
      </ScrollableTabsList>
      {Object.entries(props).map(([key, content]) => (
        <TabsContent key={key} value={key} className="mt-3">
          {content}
        </TabsContent>
      ))}
    </Tabs>
  )
}
