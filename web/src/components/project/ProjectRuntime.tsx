// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import LogViewer from '../runtime-logs/log-viewer'
import HistoryLogViewer from '../runtime-logs/history-log-viewer'
import { LogIngestBudgetWarning } from '@/components/observability/LogIngestBudgetNotice'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { FileText, Radio, Rocket, ScrollText } from 'lucide-react'
import { useNavigate, useParams } from 'react-router'
import { useUrlState } from '@temps-sdk/ds'
import { cn } from '@/lib/utils'

interface ProjectRuntimeProps {
  project: ProjectResponse
}

export function ProjectRuntime({ project }: ProjectRuntimeProps) {
  const navigate = useNavigate()
  const { slug } = useParams()
  // The tab shares its URL writes with the log search (`q`) and deployment
  // filter inside the viewers, so switching tabs right after typing keeps both.
  const { get, patch } = useUrlState<'tab'>()

  // Sync tab with ?tab= search param (defaults to "live")
  const activeTab = get('tab') === 'history' ? 'history' : 'live'

  const handleTabChange = (value: LogMode) => {
    patch({ tab: value === 'live' ? undefined : value })
  }

  // Check if project has any deployments
  if (!project.last_deployment) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <Card className="w-full max-w-md">
          <CardHeader className="text-center">
            <div className="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-lg bg-muted">
              <FileText className="h-6 w-6 text-muted-foreground" />
            </div>
            <CardTitle>No runtime logs available</CardTitle>
            <CardDescription>
              Runtime logs will appear here after your first deployment.
            </CardDescription>
          </CardHeader>
          <CardContent className="text-center">
            <Button
              onClick={() => navigate(`/projects/${slug}/deployments`)}
              className="w-full"
            >
              <Rocket className="mr-2 h-4 w-4" />
              Go to deployments
            </Button>
          </CardContent>
        </Card>
      </div>
    )
  }

  // Live and History are two modes of the same logs, not two pages: a
  // switch in the viewer's own toolbar rather than a second tab strip under
  // the Logs section tabs.
  const modeSwitch = (
    <LogModeSwitch value={activeTab} onChange={handleTabChange} />
  )
  return (
    <div className="w-full">
      <LogIngestBudgetWarning
        scope={{ projectId: project.id }}
        label={project.name}
        className="mb-3"
      />
      {activeTab === 'live' ? (
        <LogViewer project={project} modeSwitch={modeSwitch} />
      ) : (
        <HistoryLogViewer project={project} modeSwitch={modeSwitch} />
      )}
    </div>
  )
}

type LogMode = 'live' | 'history'

const LOG_MODES = [
  { value: 'live', label: 'Live', icon: Radio },
  { value: 'history', label: 'History', icon: ScrollText },
] as const

function LogModeSwitch({
  value,
  onChange,
}: {
  value: LogMode
  onChange: (value: LogMode) => void
}) {
  return (
    <div
      role="group"
      aria-label="Log mode"
      className="inline-flex h-10 shrink-0 items-center rounded-md border bg-background p-0.5"
    >
      {LOG_MODES.map((mode) => {
        const selected = value === mode.value
        const Icon = mode.icon
        return (
          <button
            key={mode.value}
            type="button"
            aria-pressed={selected}
            onClick={() => onChange(mode.value)}
            className={cn(
              'inline-flex h-full items-center gap-1.5 rounded px-3 text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
              selected
                ? 'bg-muted text-foreground'
                : 'text-muted-foreground hover:text-foreground'
            )}
          >
            <Icon className="h-3.5 w-3.5" aria-hidden="true" />
            {mode.label}
          </button>
        )
      })}
    </div>
  )
}
