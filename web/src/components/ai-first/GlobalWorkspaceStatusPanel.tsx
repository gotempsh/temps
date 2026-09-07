// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Activity,
  Cpu,
  Database,
  HardDrive,
  MemoryStick,
  Server,
  Settings2,
} from 'lucide-react'
import type { ReactNode } from 'react'
import { Link } from 'react-router'
import type { ApplicationWorkspaceResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { workspaceStatusPresentation } from './workspace-readiness'

export function GlobalWorkspaceStatusPanel({
  loading,
  waking,
  workspace,
}: {
  loading: boolean
  waking: boolean
  workspace: ApplicationWorkspaceResponse | null
}) {
  const status = workspaceStatusPresentation(workspace, loading, waking)

  return (
    <div className="space-y-4">
      <div>
        <p className="text-sm font-semibold">User workspace</p>
        <p className="mt-1 text-xs leading-5 text-muted-foreground">
          Your global AI threads share this private persistent workspace. Temps
          suspends idle compute without removing its files.
        </p>
      </div>

      <section className="space-y-3 rounded-xl border border-border bg-background p-3">
        <div className="flex items-start justify-between gap-3">
          <div className="flex min-w-0 items-center gap-2">
            <span
              aria-hidden="true"
              className={`size-2 shrink-0 rounded-full ${status.dot}`}
            />
            <div className="min-w-0">
              <p className="text-sm font-medium">{status.label}</p>
              <p className="truncate font-mono text-[10px] text-muted-foreground">
                {workspace?.sandbox_public_id ?? 'Not created yet'}
              </p>
            </div>
          </div>
          {workspace && (
            <span className="rounded-full bg-muted px-2 py-1 font-mono text-[9px] text-muted-foreground">
              {workspace.runtime}
            </span>
          )}
        </div>
        <p className="text-[11px] leading-5 text-muted-foreground">
          {status.detail}
        </p>

        {workspace && (
          <>
            <div className="grid grid-cols-2 gap-2 border-t border-border pt-3">
              <Metric
                icon={<MemoryStick className="size-3.5" />}
                label="Memory"
                value={`${formatBytes(workspace.memory_used_bytes)} / ${workspace.memory_limit_mb} MB`}
              />
              <Metric
                icon={<Cpu className="size-3.5" />}
                label="CPU time"
                value={formatCpu(workspace.cpu_usage_usec)}
              />
              <Metric
                icon={<Activity className="size-3.5" />}
                label="Processes"
                value={`${workspace.pids_used ?? '—'} / ${workspace.pids_limit}`}
              />
              <Metric
                icon={<HardDrive className="size-3.5" />}
                label="Disk"
                value={`${formatBytes(workspace.disk_used_bytes)} / ${workspace.disk_limit_mb} MB`}
              />
            </div>
            <div className="space-y-2 border-t border-border pt-3 text-[11px]">
              <StatusLine
                icon={<Server className="size-3.5" />}
                label="Runtime image"
                value={workspace.image ?? 'Temps managed image'}
              />
              <StatusLine
                icon={<Database className="size-3.5" />}
                label="Data services"
                value={String(workspace.data_network_service_count)}
              />
              <StatusLine
                icon={<HardDrive className="size-3.5" />}
                label="Persistent volume"
                value={
                  workspace.persistent_volume_healthy
                    ? 'Healthy'
                    : 'Needs attention'
                }
              />
            </div>
          </>
        )}
      </section>

      <section className="space-y-3 rounded-xl border border-border p-3">
        <div>
          <p className="text-xs font-medium">Sandbox configuration</p>
          <p className="mt-1 text-[10px] leading-4 text-muted-foreground">
            Configure the runtime image, resource defaults, and secure sandbox
            backend used when managed workspaces are created or rebuilt.
          </p>
        </div>
        <Button asChild className="w-full" size="sm" variant="outline">
          <Link to="/agent-sandbox/sandbox">
            <Settings2 className="mr-1.5 size-3.5" /> Open sandbox settings
          </Link>
        </Button>
      </section>

      <p className="text-[10px] leading-4 text-muted-foreground">
        Managed AI workspaces are intentionally separate from standalone
        sandboxes so every operation can re-check your current platform access.
      </p>
    </div>
  )
}

function Metric({
  icon,
  label,
  value,
}: {
  icon: ReactNode
  label: string
  value: string
}) {
  return (
    <div className="rounded-lg bg-muted/60 p-2">
      <div className="flex items-center gap-1.5 text-[10px] text-muted-foreground">
        {icon} {label}
      </div>
      <p className="mt-1 truncate text-xs font-medium">{value}</p>
    </div>
  )
}

function StatusLine({
  icon,
  label,
  value,
}: {
  icon: ReactNode
  label: string
  value: string
}) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className="flex items-center gap-1.5 text-muted-foreground">
        {icon} {label}
      </span>
      <span className="min-w-0 truncate font-medium">{value}</span>
    </div>
  )
}

function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null) return '—'
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`
  return `${Math.round(bytes / (1024 * 1024))} MB`
}

function formatCpu(microseconds: number | null | undefined): string {
  if (microseconds == null) return '—'
  if (microseconds < 1_000_000) return `${Math.round(microseconds / 1000)} ms`
  return `${(microseconds / 1_000_000).toFixed(1)} s`
}
