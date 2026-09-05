// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Activity,
  Archive,
  ArchiveRestore,
  Cpu,
  Database,
  HardDrive,
  Loader2,
  MemoryStick,
  Pause,
  Play,
  RefreshCw,
  RotateCw,
  Save,
  Server,
} from 'lucide-react'
import {
  useCallback,
  useEffect,
  useState,
  type Dispatch,
  type ReactNode,
  type SetStateAction,
} from 'react'
import {
  controlApplicationWorkspace,
  getApplicationWorkspace,
  updateApplicationWorkspace,
  type ApplicationWorkspaceResponse,
} from '@/api/client'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'

type Props = {
  applicationPublicId: string
  initialWorkspace?: ApplicationWorkspaceResponse | null
  onWorkspaceChange?: (workspace: ApplicationWorkspaceResponse) => void
  waking?: boolean
}

export function ApplicationWorkspaceSettingsPanel({
  applicationPublicId,
  initialWorkspace = null,
  onWorkspaceChange,
  waking = false,
}: Props) {
  const [workspace, setWorkspace] =
    useState<ApplicationWorkspaceResponse | null>(initialWorkspace)
  const [loading, setLoading] = useState(initialWorkspace == null)
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [snapshotId, setSnapshotId] = useState('')
  const [form, setForm] = useState({
    runtime: 'node',
    cpu_limit: '2',
    memory_limit_mb: '4096',
    pids_limit: '1024',
    disk_limit_mb: '20480',
    idle_timeout_secs: '86400',
  })

  const acceptWorkspace = useCallback(
    (next: ApplicationWorkspaceResponse, notifyParent = true) => {
      setWorkspace(next)
      if (notifyParent) onWorkspaceChange?.(next)
      setForm({
        runtime: next.runtime,
        cpu_limit: String(next.cpu_limit),
        memory_limit_mb: String(next.memory_limit_mb),
        pids_limit: String(next.pids_limit),
        disk_limit_mb: String(next.disk_limit_mb),
        idle_timeout_secs: String(next.idle_timeout_secs),
      })
      if (next.snapshot_id) setSnapshotId(next.snapshot_id)
    },
    [onWorkspaceChange]
  )

  const load = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const { data } = await getApplicationWorkspace({
        path: { application_public_id: applicationPublicId },
        throwOnError: true,
      })
      acceptWorkspace(data)
    } catch (cause) {
      setError(errorMessage(cause, 'Could not load workspace status.'))
    } finally {
      setLoading(false)
    }
  }, [acceptWorkspace, applicationPublicId])

  useEffect(() => {
    if (initialWorkspace) {
      const syncTimer = window.setTimeout(() => {
        acceptWorkspace(initialWorkspace, false)
        setLoading(false)
      }, 0)
      return () => window.clearTimeout(syncTimer)
    }
    const timeout = window.setTimeout(() => void load(), 0)
    return () => window.clearTimeout(timeout)
  }, [acceptWorkspace, initialWorkspace, load])

  const control = async (action: string) => {
    setBusy(action)
    setError(null)
    try {
      const { data } = await controlApplicationWorkspace({
        path: { application_public_id: applicationPublicId },
        body: {
          action,
          snapshot_id: action === 'restore' ? snapshotId.trim() : null,
          label: action === 'snapshot' ? 'Application workspace' : null,
        },
        throwOnError: true,
      })
      acceptWorkspace(data)
    } catch (cause) {
      setError(errorMessage(cause, `Could not ${action} the workspace.`))
    } finally {
      setBusy(null)
    }
  }

  const save = async () => {
    setBusy('save')
    setError(null)
    try {
      const { data } = await updateApplicationWorkspace({
        path: { application_public_id: applicationPublicId },
        body: {
          runtime: form.runtime,
          cpu_limit: Number(form.cpu_limit),
          memory_limit_mb: Number(form.memory_limit_mb),
          pids_limit: Number(form.pids_limit),
          disk_limit_mb: Number(form.disk_limit_mb),
          idle_timeout_secs: Number(form.idle_timeout_secs),
        },
        throwOnError: true,
      })
      acceptWorkspace(data)
    } catch (cause) {
      setError(errorMessage(cause, 'Could not save workspace resources.'))
    } finally {
      setBusy(null)
    }
  }

  if (loading && !workspace) {
    return (
      <div className="flex items-center gap-2 py-8 text-xs text-muted-foreground">
        <Loader2 className="size-4 animate-spin" /> Checking sandbox status…
      </div>
    )
  }

  return (
    <div className="space-y-5">
      <div className="flex items-start justify-between gap-3">
        <div>
          <p className="text-sm font-semibold">Persistent workspace</p>
          <p className="mt-1 text-xs leading-5 text-muted-foreground">
            Files persist independently from compute. Sleeping or rebuilding the
            sandbox keeps the application volume.
          </p>
        </div>
        <Button
          aria-label="Refresh workspace"
          disabled={loading || busy !== null}
          onClick={() => void load()}
          size="icon"
          variant="ghost"
        >
          <RefreshCw className="size-3.5" />
        </Button>
      </div>

      {waking && (
        <div className="flex items-center gap-2 rounded-lg border border-amber-500/30 bg-amber-500/5 px-3 py-3 text-xs text-amber-700 dark:text-amber-300">
          <Loader2 className="size-3.5 animate-spin" />
          Sandbox waking up. Persistent files are already safe; controls and
          live usage will become available after the accessibility check.
        </div>
      )}

      {workspace && (
        <>
          <section className="rounded-xl border border-border bg-background p-3">
            <div className="flex items-center justify-between gap-3">
              <div className="flex items-center gap-2">
                <span
                  className={`size-2 rounded-full ${stateColor(workspace.state)}`}
                />
                <div>
                  <p className="text-sm font-medium capitalize">
                    {workspace.state}
                  </p>
                  <p className="font-mono text-[10px] text-muted-foreground">
                    desired: {workspace.desired_state}
                  </p>
                </div>
              </div>
              <span className="rounded-full bg-muted px-2 py-1 font-mono text-[9px] text-muted-foreground">
                {workspace.runtime}
              </span>
            </div>
            <div className="mt-3 grid grid-cols-2 gap-2">
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
                value={`${formatBytes(workspace.disk_used_bytes)} / ${workspace.disk_limit_mb} MB${workspace.disk_limit_enforced ? '' : ' desired'}`}
              />
            </div>
            <div className="mt-3 space-y-2 border-t border-border pt-3 text-[11px]">
              <StatusLine
                icon={<Server className="size-3.5" />}
                label="Runtime image"
                value={workspace.image ?? 'Temps managed image'}
              />
              <StatusLine
                icon={<Database className="size-3.5" />}
                label="Reachable data services"
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
              {!workspace.disk_limit_enforced && (
                <p className="rounded-md bg-muted/60 p-2 text-[10px] leading-4 text-muted-foreground">
                  Docker workspaces report disk usage but cannot enforce a
                  per-directory quota. VM runtimes enforce the desired disk
                  limit.
                </p>
              )}
              <StatusLine
                icon={<Activity className="size-3.5" />}
                label="Preview ports"
                value={
                  workspace.open_preview_ports.length > 0
                    ? workspace.open_preview_ports.join(', ')
                    : 'None detected'
                }
              />
            </div>
          </section>

          <section className="space-y-3 rounded-xl border border-border p-3">
            <p className="text-xs font-medium">Lifecycle</p>
            <div className="grid grid-cols-2 gap-2">
              <ActionButton
                action="restart"
                busy={busy}
                icon={<RotateCw className="size-3.5" />}
                onClick={control}
              />
              <ActionButton
                action="rebuild"
                busy={busy}
                icon={<RefreshCw className="size-3.5" />}
                onClick={control}
              />
              {workspace.desired_state === 'paused' ? (
                <ActionButton
                  action="resume"
                  busy={busy}
                  icon={<Play className="size-3.5" />}
                  onClick={control}
                />
              ) : (
                <ActionButton
                  action="pause"
                  busy={busy}
                  icon={<Pause className="size-3.5" />}
                  onClick={control}
                />
              )}
              <ActionButton
                action="snapshot"
                busy={busy}
                icon={<Archive className="size-3.5" />}
                onClick={control}
              />
            </div>
            <div className="flex gap-2 border-t border-border pt-3">
              <Input
                aria-label="Snapshot ID"
                onChange={(event) => setSnapshotId(event.target.value)}
                placeholder="Snapshot ID to restore"
                value={snapshotId}
              />
              <Button
                disabled={busy !== null || !snapshotId.trim()}
                onClick={() => void control('restore')}
                size="sm"
                variant="outline"
              >
                {busy === 'restore' ? (
                  <Loader2 className="mr-1 size-3.5 animate-spin" />
                ) : (
                  <ArchiveRestore className="mr-1 size-3.5" />
                )}
                Restore
              </Button>
            </div>
          </section>

          <section className="space-y-3 rounded-xl border border-border p-3">
            <p className="text-xs font-medium">Desired resources</p>
            <div className="grid grid-cols-2 gap-3">
              <Field label="Runtime">
                <select
                  className="h-9 w-full rounded-md border border-input bg-background px-2 text-xs"
                  onChange={(event) =>
                    setForm((current) => ({
                      ...current,
                      runtime: event.target.value,
                    }))
                  }
                  value={form.runtime}
                >
                  {['node', 'bun', 'python', 'rust', 'go', 'full'].map(
                    (runtime) => (
                      <option key={runtime} value={runtime}>
                        {runtime}
                      </option>
                    )
                  )}
                </select>
              </Field>
              <Field label="CPU cores">
                <ResourceInput
                  max="8"
                  min="0.25"
                  name="cpu_limit"
                  step="0.25"
                  value={form.cpu_limit}
                  onChange={setForm}
                />
              </Field>
              <Field label="Memory MB">
                <ResourceInput
                  max="16384"
                  min="256"
                  name="memory_limit_mb"
                  value={form.memory_limit_mb}
                  onChange={setForm}
                />
              </Field>
              <Field label="PID limit">
                <ResourceInput
                  max="2048"
                  min="64"
                  name="pids_limit"
                  value={form.pids_limit}
                  onChange={setForm}
                />
              </Field>
              <Field label="Disk MB">
                <ResourceInput
                  max="65536"
                  min="512"
                  name="disk_limit_mb"
                  value={form.disk_limit_mb}
                  onChange={setForm}
                />
              </Field>
              <Field label="Idle timeout seconds">
                <ResourceInput
                  max="86400"
                  min="60"
                  name="idle_timeout_secs"
                  value={form.idle_timeout_secs}
                  onChange={setForm}
                />
              </Field>
            </div>
            <p className="text-[10px] leading-4 text-muted-foreground">
              Runtime images are managed and pinned by Temps so application
              files and turn-scoped credentials are never mounted into an
              untrusted container image.
            </p>
            <Button
              className="w-full"
              disabled={busy !== null}
              onClick={() => void save()}
              size="sm"
            >
              {busy === 'save' ? (
                <Loader2 className="mr-1 size-3.5 animate-spin" />
              ) : (
                <Save className="mr-1 size-3.5" />
              )}
              Save and apply
            </Button>
          </section>
        </>
      )}

      {(error || workspace?.last_error) && (
        <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-xs leading-5 text-destructive">
          {error ?? workspace?.last_error}
        </div>
      )}
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

function ActionButton({
  action,
  busy,
  icon,
  onClick,
}: {
  action: string
  busy: string | null
  icon: ReactNode
  onClick: (action: string) => Promise<void>
}) {
  return (
    <Button
      className="justify-start capitalize"
      disabled={busy !== null}
      onClick={() => void onClick(action)}
      size="sm"
      variant="outline"
    >
      {busy === action ? (
        <Loader2 className="mr-1.5 size-3.5 animate-spin" />
      ) : (
        <span className="mr-1.5">{icon}</span>
      )}
      {action}
    </Button>
  )
}

function Field({ children, label }: { children: ReactNode; label: string }) {
  return (
    <div className="space-y-1.5">
      <Label>{label}</Label>
      {children}
    </div>
  )
}

type ResourceForm = {
  runtime: string
  cpu_limit: string
  memory_limit_mb: string
  pids_limit: string
  disk_limit_mb: string
  idle_timeout_secs: string
}

function ResourceInput({
  max,
  min,
  name,
  onChange,
  step,
  value,
}: {
  max: string
  min: string
  name: keyof ResourceForm
  onChange: Dispatch<SetStateAction<ResourceForm>>
  step?: string
  value: string
}) {
  return (
    <Input
      max={max}
      min={min}
      onChange={(event) =>
        onChange((current) => ({ ...current, [name]: event.target.value }))
      }
      step={step}
      type="number"
      value={value}
    />
  )
}

function stateColor(state: string): string {
  if (state === 'running') return 'bg-success'
  if (state === 'failed') return 'bg-destructive'
  if (state === 'recovering') return 'bg-amber-500'
  return 'bg-muted-foreground'
}

function formatBytes(value: number | null | undefined): string {
  if (value == null) return '—'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let scaled = value
  let unit = 0
  while (scaled >= 1024 && unit < units.length - 1) {
    scaled /= 1024
    unit += 1
  }
  return `${scaled >= 10 || unit === 0 ? scaled.toFixed(0) : scaled.toFixed(1)} ${units[unit]}`
}

function formatCpu(value: number | null | undefined): string {
  if (value == null) return '—'
  return `${(value / 1_000_000).toFixed(1)} s`
}

function errorMessage(cause: unknown, fallback: string): string {
  if (cause instanceof Error && cause.message.trim()) return cause.message
  return fallback
}
