import { useEffect, useMemo, useRef, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import {
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import {
  ArrowLeft,
  Box,
  ChevronDown,
  ChevronRight,
  Cpu,
  ExternalLink,
  GitBranch,
  HardDrive,
  KeyRound,
  Loader2,
  Play,
  Plus,
  RefreshCw,
  RotateCw,
  Square,
  Terminal,
  Timer,
  Trash2,
} from 'lucide-react'
import { toast } from 'sonner'

import { usePageTitle } from '@/hooks/usePageTitle'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import {
  execMutation,
  extendTimeoutMutation,
  getSandboxOptions,
  listEventsOptions,
  listJobsOptions,
  pauseSandboxMutation,
  resizeSandboxMutation,
  restartSandboxMutation,
  resumeSandboxMutation,
  stopSandboxMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { jobStatus } from '@/api/client/sdk.gen'
import type { ExecResponse, SandboxEvent } from '@/api/client/types.gen'
import {
  jobLogsUrl,
  toSandboxView,
  type JobSummary,
} from '@/components/sandboxes/helpers'
import { SandboxPreviewPasswordCard } from '@/components/sandboxes/SandboxPreviewPasswordCard'

function statusVariant(
  status: string,
): 'default' | 'secondary' | 'success' | 'warning' | 'destructive' | 'outline' {
  switch (status) {
    case 'running':
      return 'success'
    case 'stopped':
      return 'warning'
    case 'destroyed':
      return 'destructive'
    default:
      return 'outline'
  }
}

// Presentation for each timeline event type: icon, human label, and an
// optional one-line detail derived from the event's structured payload.
function eventPresentation(e: SandboxEvent): {
  icon: typeof Plus
  label: string
  detail?: string
} {
  const d = (e.detail ?? {}) as Record<string, unknown>
  switch (e.event_type) {
    case 'created':
      return {
        icon: Plus,
        label: 'Created',
        detail: [d.backend, d.image].filter(Boolean).join(' · ') || undefined,
      }
    case 'stopped':
      return { icon: Square, label: 'Stopped' }
    case 'resumed':
      return { icon: Play, label: 'Resumed' }
    case 'restarted':
      return { icon: RotateCw, label: 'Restarted' }
    case 'timeout_extended':
      return {
        icon: Timer,
        label: 'Timeout extended',
        detail:
          typeof d.extra_secs === 'number'
            ? `+${Math.round((d.extra_secs as number) / 60)}m`
            : undefined,
      }
    case 'resized':
      return {
        icon: HardDrive,
        label: 'Disk resized',
        detail:
          d.from_mb && d.to_mb ? `${d.from_mb} → ${d.to_mb} MiB` : undefined,
      }
    case 'preview_password_set':
      return { icon: KeyRound, label: 'Preview password set' }
    case 'preview_password_cleared':
      return { icon: KeyRound, label: 'Preview password cleared' }
    case 'source_seeded':
      return {
        icon: GitBranch,
        label: 'Source seeded',
        detail: typeof d.type === 'string' ? (d.type as string) : undefined,
      }
    case 'destroyed':
      return { icon: Trash2, label: 'Destroyed' }
    default:
      return { icon: Terminal, label: e.event_type }
  }
}

// Dev-server defaults the Open Preview dropdown surfaces as quick picks.
// Ordered by popularity so Next.js (3000) and Vite (5173) land on top.
const DEFAULT_PORTS: { port: number; label: string }[] = [
  { port: 3000, label: 'Next.js · Node' },
  { port: 5173, label: 'Vite' },
  { port: 8080, label: 'Generic HTTP' },
  { port: 8000, label: 'Django · FastAPI' },
  { port: 4000, label: 'Phoenix · Keystone' },
  { port: 4200, label: 'Angular' },
  { port: 3001, label: 'Alt Node' },
]

// Preset exec commands. Each becomes a one-click chip. Chosen for the
// most common "what's in here / what's running" questions an operator
// asks without typing anything. All run through `sh -c` like the free-form
// box (see `runExec`), so pipes, `||`, and globs work. `shell: false` opts
// a chip out into argv-style exec if one ever needs the args verbatim.
const PRESET_CMDS: {
  label: string
  cmd: string
  hint?: string
  shell?: boolean
}[] = [
  { label: 'ls', cmd: 'ls -la', hint: 'List files' },
  { label: 'ps', cmd: 'ps auxf', hint: 'Running processes' },
  { label: 'env', cmd: 'env', hint: 'Environment variables' },
  { label: 'pwd', cmd: 'pwd', hint: 'Working directory' },
  { label: 'disk', cmd: 'df -h', hint: 'Disk usage' },
  { label: 'package.json', cmd: 'cat package.json', hint: 'Show package.json' },
  {
    label: 'ports',
    // /proc/net/tcp is always present in Linux containers (no extra pkgs).
    // awk parses the hex LISTEN rows (state 0A) into decimal port numbers.
    cmd: "awk 'NR>1 && $4==\"0A\"{split($2,a,\":\"); print strtonum(\"0x\"a[2])}' /proc/net/tcp /proc/net/tcp6 2>/dev/null | sort -un",
    hint: 'Listening TCP ports',
  },
]

function formatDate(iso: string): string {
  try {
    return new Date(iso).toLocaleString()
  } catch {
    return iso
  }
}

function formatCountdown(iso: string, now: number): string {
  const diffMs = new Date(iso).getTime() - now
  if (diffMs <= 0) return 'expired'
  const secs = Math.floor(diffMs / 1000)
  const d = Math.floor(secs / 86400)
  const h = Math.floor((secs % 86400) / 3600)
  const m = Math.floor((secs % 3600) / 60)
  const s = secs % 60
  if (d >= 1) return `${d}d ${h}h`
  if (h >= 1) return `${h}h ${m}m`
  if (m >= 1) return `${m}m ${s}s`
  return `${s}s`
}

function formatAge(iso: string, now: number): string {
  const diffMs = now - new Date(iso).getTime()
  if (diffMs < 0) return formatDate(iso)
  const secs = Math.floor(diffMs / 1000)
  if (secs < 60) return `${secs}s ago`
  const mins = Math.floor(secs / 60)
  if (mins < 60) return `${mins}m ago`
  const hours = Math.floor(mins / 60)
  if (hours < 24) return `${hours}h ago`
  const days = Math.floor(hours / 24)
  return `${days}d ago`
}

function parseCommand(input: string): string[] {
  // Minimal shell-ish splitter: supports double quotes for grouping.
  const out: string[] = []
  let buf = ''
  let inQuote = false
  for (let i = 0; i < input.length; i++) {
    const ch = input[i]
    if (ch === '"') {
      inQuote = !inQuote
      continue
    }
    if (!inQuote && /\s/.test(ch)) {
      if (buf.length > 0) {
        out.push(buf)
        buf = ''
      }
      continue
    }
    buf += ch
  }
  if (buf.length > 0) out.push(buf)
  return out
}

/// Tick every second so expiry countdown & last-activity feel live.
/// Using a single top-level tick avoids each row setting its own interval.
function useNow(enabled: boolean) {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!enabled) return
    const id = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(id)
  }, [enabled])
  return now
}

export default function SandboxDetail() {
  usePageTitle('Sandbox')
  const { sandboxId = '' } = useParams<{ sandboxId: string }>()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [deleteOpen, setDeleteOpen] = useState(false)
  const [resizeOpen, setResizeOpen] = useState(false)
  const [resizeInput, setResizeInput] = useState('')
  const [cmdInput, setCmdInput] = useState('')
  const [cwdInput, setCwdInput] = useState('')
  const [execResult, setExecResult] = useState<ExecResponse | null>(null)
  const [execDuration, setExecDuration] = useState<number | null>(null)
  const [execStartedCmd, setExecStartedCmd] = useState<string | null>(null)
  const [customPort, setCustomPort] = useState('')
  const cmdInputRef = useRef<HTMLInputElement | null>(null)

  const sandboxQuery = getSandboxOptions({ path: { id: sandboxId } })
  const {
    data: sandboxResp,
    isLoading,
    isError,
    error,
    refetch,
    isFetching,
  } = useQuery({
    ...sandboxQuery,
    enabled: sandboxId.length > 0,
    refetchInterval: 10_000,
  })
  const sandbox = sandboxResp ? toSandboxView(sandboxResp.sandbox) : undefined

  const now = useNow(!!sandbox && sandbox.status !== 'destroyed')

  // Detached jobs (background execs). Polls every 3s while the sandbox is
  // alive so terminal state flips quickly; stops polling for destroyed
  // sandboxes so we don't hammer a 404 loop. The backend holds these in
  // memory — restarting the control plane clears them, which matches the
  // ephemeral Command contract the SDK expects.
  const jobsEnabled = !!sandbox && sandbox.status !== 'destroyed'
  const jobsQuery = listJobsOptions({ path: { id: sandboxId } })
  const { data: jobsResp } = useQuery({
    ...jobsQuery,
    enabled: jobsEnabled,
    refetchInterval: jobsEnabled ? 3_000 : false,
  })
  const jobs: JobSummary[] = jobsResp?.items ?? []

  // Operations timeline (lifecycle events, not shell). Polls while alive so
  // a stop/resize/extend shows up without a manual refresh.
  const { data: eventsResp } = useQuery({
    ...listEventsOptions({ path: { id: sandboxId } }),
    enabled: sandboxId.length > 0,
    refetchInterval: jobsEnabled ? 5_000 : false,
  })
  const events: SandboxEvent[] = eventsResp?.events ?? []

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ['sandbox', sandboxId] })
    queryClient.invalidateQueries({ queryKey: ['sandboxes'] })
    queryClient.invalidateQueries({ queryKey: ['sandbox-events', sandboxId] })
  }

  const resizeMutation = useMutation({
    ...resizeSandboxMutation(),
    meta: { errorTitle: 'Failed to resize disk' },
    onSuccess: () => {
      invalidate()
      setResizeOpen(false)
      setResizeInput('')
      toast.success('Disk resized — sandbox restarted')
    },
  })

  const deleteMutation = useMutation({
    ...stopSandboxMutation(),
    meta: { errorTitle: 'Failed to delete sandbox' },
    onSuccess: () => {
      invalidate()
      setDeleteOpen(false)
      toast.success('Sandbox deleted')
      navigate('/sandboxes')
    },
  })

  const pauseMutationHook = useMutation({
    ...pauseSandboxMutation(),
    meta: { errorTitle: 'Failed to stop sandbox' },
    onSuccess: () => {
      invalidate()
      toast.success('Sandbox stopped')
    },
  })

  const resumeMutationHook = useMutation({
    ...resumeSandboxMutation(),
    meta: { errorTitle: 'Failed to resume sandbox' },
    onSuccess: () => {
      invalidate()
      toast.success('Sandbox resumed')
    },
  })

  const restartMutationHook = useMutation({
    ...restartSandboxMutation(),
    meta: { errorTitle: 'Failed to restart sandbox' },
    onSuccess: () => {
      invalidate()
      toast.success('Sandbox restarted')
    },
  })

  const extendMutationHook = useMutation({
    ...extendTimeoutMutation(),
    meta: { errorTitle: 'Failed to extend timeout' },
    onSuccess: (_data, vars) => {
      invalidate()
      const secs = vars.body?.extra_secs ?? 0
      toast.success(
        `Timeout extended by ${secs >= 3600 ? `${secs / 3600}h` : `${secs / 60}m`}`,
      )
    },
  })

  // A `$`-prompt implies shell semantics, so the free-form box runs through
  // `sh -c` by default — redirections (`>`), `&&`, pipes, and globs all
  // behave as typed. Pass `shell: false` to exec a command argv-style
  // (bypassing the shell) when a caller needs the raw args verbatim.
  const execMutationHook = useMutation({
    ...execMutation(),
    meta: { errorTitle: 'Command failed' },
    onSuccess: (result) => {
      setExecResult(result)
      invalidate()
    },
    onError: (err: Error) => {
      setExecDuration(null)
      setExecResult({ exit_code: -1, stdout: '', stderr: err.message })
    },
  })

  const runExec = (args?: { cmd?: string; shell?: boolean }) => {
    const raw = args?.cmd ?? cmdInput
    // Shell by default (see execMutationHook note); opt out with shell:false.
    const useShell = args?.shell ?? true
    const cmd = useShell ? ['sh', '-c', raw] : parseCommand(raw)
    if (cmd.length === 0) return
    const started = performance.now()
    setExecStartedCmd(raw)
    execMutationHook.mutate(
      {
        path: { id: sandboxId },
        body: { cmd, cwd: cwdInput.trim() || undefined },
      },
      {
        onSettled: () =>
          setExecDuration(Math.round(performance.now() - started)),
      },
    )
  }

  // Open a port in a new tab. `target=_blank` + `noopener` per usual
  // rel safety. We defensively validate the port so a stray click on
  // a malformed custom port never produces a bogus URL.
  const openPort = (port: number) => {
    if (!sandbox?.preview_url_template || port < 1 || port > 65535) return
    const url = sandbox.preview_url_template.replace('{port}', String(port))
    window.open(url, '_blank', 'noopener,noreferrer')
  }

  const customPortValid = useMemo(() => {
    if (!/^\d+$/.test(customPort)) return false
    const n = Number(customPort)
    return n >= 1 && n <= 65535
  }, [customPort])

  if (isLoading) {
    return (
      <div className="w-full p-4 sm:p-6 lg:p-8 space-y-6">
        <div className="h-4 w-40 rounded bg-muted animate-pulse" />
        <div className="flex flex-col gap-4 lg:flex-row lg:items-end lg:justify-between">
          <div className="space-y-2">
            <div className="h-7 w-64 rounded bg-muted animate-pulse" />
            <div className="h-3 w-48 rounded bg-muted animate-pulse" />
          </div>
          <div className="flex gap-2">
            <div className="h-9 w-32 rounded bg-muted animate-pulse" />
            <div className="h-9 w-24 rounded bg-muted animate-pulse" />
          </div>
        </div>
        <Card>
          <CardContent className="py-4">
            <div className="h-10 w-full rounded bg-muted animate-pulse" />
          </CardContent>
        </Card>
        <Card>
          <CardContent className="py-6 space-y-3">
            <div className="h-8 w-full rounded bg-muted animate-pulse" />
            <div className="h-20 w-full rounded bg-muted animate-pulse" />
          </CardContent>
        </Card>
      </div>
    )
  }

  if (isError || !sandbox) {
    return (
      <div className="w-full p-4 sm:p-6 lg:p-8 space-y-4">
        <Button variant="ghost" size="sm" onClick={() => navigate('/sandboxes')}>
          <ArrowLeft className="mr-1.5 h-4 w-4" />
          Back to sandboxes
        </Button>
        <Card>
          <CardContent className="py-12 text-center space-y-2">
            <Box className="mx-auto h-8 w-8 text-destructive" />
            <p className="text-sm font-medium">Sandbox not found</p>
            <p className="text-xs text-muted-foreground">
              {(error as Error)?.message ?? 'This sandbox may have been deleted.'}
            </p>
          </CardContent>
        </Card>
      </div>
    )
  }

  const running = sandbox.status === 'running'
  const stopped = sandbox.status === 'stopped'
  const destroyed = sandbox.status === 'destroyed'
  const hasPreview = Boolean(sandbox.preview_url_template) && running

  const timeLeft = !destroyed ? formatCountdown(sandbox.expires_at, now) : '—'
  const expired = !destroyed && new Date(sandbox.expires_at).getTime() <= now

  return (
    <div className="w-full p-4 sm:p-6 lg:p-8 space-y-6">
      {/* Breadcrumb */}
      <div className="flex items-center gap-1 text-sm text-muted-foreground">
        <Link to="/sandboxes" className="hover:text-foreground">
          Sandboxes
        </Link>
        <ChevronRight className="h-4 w-4" />
        <span className="font-mono text-foreground truncate">{sandbox.id}</span>
      </div>

      {/* Header: identity + primary actions (DESIGN.md §4.4) */}
      <div className="flex flex-col gap-4 lg:flex-row lg:items-end lg:justify-between">
        <div className="min-w-0 space-y-1">
          <h1 className="text-2xl font-semibold tracking-tight flex items-center gap-2 flex-wrap">
            <span className="truncate">{sandbox.name}</span>
            <Badge variant={statusVariant(sandbox.status)}>
              {sandbox.status}
            </Badge>
            {sandbox.backend && (
              <Badge
                variant="secondary"
                className="gap-1 font-normal"
                title={
                  sandbox.backend === 'firecracker'
                    ? 'Hardware-virtualized microVM (KVM)'
                    : 'Namespaced container'
                }
              >
                {sandbox.backend === 'firecracker' ? (
                  <Cpu className="h-3 w-3" />
                ) : (
                  <Box className="h-3 w-3" />
                )}
                {sandbox.backend === 'firecracker'
                  ? 'Firecracker'
                  : sandbox.backend === 'docker'
                    ? 'Docker'
                    : sandbox.backend}
              </Badge>
            )}
          </h1>
          <div className="flex items-center gap-2 text-xs font-mono text-muted-foreground">
            <span className="truncate">{sandbox.id}</span>
            <CopyButton value={sandbox.id} minimal className="h-5 w-5 shrink-0" />
          </div>
        </div>

        {/* Primary action cluster — what the user actually came here to do */}
        <div className="flex flex-wrap items-center gap-2">
          {hasPreview && (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button size="sm" className="gap-1">
                  <ExternalLink className="h-4 w-4" />
                  Open preview
                  <ChevronDown className="h-3 w-3 opacity-70" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-56">
                {DEFAULT_PORTS.map((p) => (
                  <DropdownMenuItem
                    key={p.port}
                    onClick={() => openPort(p.port)}
                    className="justify-between"
                  >
                    <span className="font-mono text-xs">:{p.port}</span>
                    <span className="text-muted-foreground text-xs">
                      {p.label}
                    </span>
                  </DropdownMenuItem>
                ))}
                <DropdownMenuSeparator />
                <div className="p-2 space-y-1.5">
                  <Label
                    htmlFor="dd-custom-port"
                    className="text-xs text-muted-foreground"
                  >
                    Custom port
                  </Label>
                  <form
                    className="flex items-center gap-1.5"
                    onSubmit={(e) => {
                      e.preventDefault()
                      if (customPortValid) openPort(Number(customPort))
                    }}
                  >
                    <Input
                      id="dd-custom-port"
                      type="number"
                      min={1}
                      max={65535}
                      placeholder="4321"
                      value={customPort}
                      onChange={(e) => setCustomPort(e.target.value)}
                      className="h-8 text-xs"
                    />
                    <Button
                      type="submit"
                      size="sm"
                      variant="outline"
                      className="h-8"
                      disabled={!customPortValid}
                    >
                      Go
                    </Button>
                  </form>
                </div>
              </DropdownMenuContent>
            </DropdownMenu>
          )}

          {running && (
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                pauseMutationHook.mutate({ path: { id: sandboxId } })
              }
              disabled={pauseMutationHook.isPending}
            >
              <Square className="mr-1 h-4 w-4" />
              Stop
            </Button>
          )}
          {stopped && (
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                resumeMutationHook.mutate({ path: { id: sandboxId } })
              }
              disabled={resumeMutationHook.isPending}
            >
              <Play className="mr-1 h-4 w-4" />
              Resume
            </Button>
          )}
          {running && (
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                restartMutationHook.mutate({ path: { id: sandboxId } })
              }
              disabled={restartMutationHook.isPending}
              title="Restart"
            >
              <RotateCw className="h-4 w-4" />
            </Button>
          )}
          <Button
            variant="outline"
            size="sm"
            onClick={() => refetch()}
            disabled={isFetching}
            title="Refresh"
          >
            <RefreshCw className={`h-4 w-4 ${isFetching ? 'animate-spin' : ''}`} />
          </Button>
          {!destroyed && (
            <Button
              variant="outline"
              size="sm"
              onClick={() => setDeleteOpen(true)}
              className="text-destructive hover:text-destructive"
              title="Delete"
            >
              <Trash2 className="h-4 w-4" />
            </Button>
          )}
        </div>
      </div>

      {/* Live status strip — countdown with inline extend actions */}
      {!destroyed && (
        <Card className={expired ? 'border-destructive/40' : undefined}>
          <CardContent className="py-4">
            <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
              <div className="flex items-center gap-4 flex-wrap">
                <div className="flex items-center gap-2">
                  <Timer
                    className={`h-4 w-4 ${
                      expired ? 'text-destructive' : 'text-muted-foreground'
                    }`}
                  />
                  <div className="leading-tight">
                    <div
                      className={`font-mono text-sm tabular-nums ${
                        expired ? 'text-destructive' : ''
                      }`}
                    >
                      {expired ? 'expired' : `${timeLeft} left`}
                    </div>
                    <div className="text-xs text-muted-foreground">
                      expires {formatDate(sandbox.expires_at)}
                    </div>
                  </div>
                </div>
                <div className="h-8 w-px bg-border hidden sm:block" />
                <div className="leading-tight">
                  <div className="text-sm tabular-nums">{formatAge(sandbox.created_at, now)}</div>
                  <div className="text-xs text-muted-foreground">
                    created
                  </div>
                </div>
              </div>
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="text-xs text-muted-foreground mr-1">
                  Extend:
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    extendMutationHook.mutate({
                      path: { id: sandboxId },
                      body: { extra_secs: 900 },
                    })
                  }
                  disabled={extendMutationHook.isPending}
                >
                  +15m
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    extendMutationHook.mutate({
                      path: { id: sandboxId },
                      body: { extra_secs: 3600 },
                    })
                  }
                  disabled={extendMutationHook.isPending}
                >
                  +1h
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    extendMutationHook.mutate({
                      path: { id: sandboxId },
                      body: { extra_secs: 14400 },
                    })
                  }
                  disabled={extendMutationHook.isPending}
                >
                  +4h
                </Button>
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      {/* Command workbench — primary action, always visible */}
      <Card>
        <CardHeader className="pb-3">
          <div className="flex items-center justify-between gap-2">
            <CardTitle className="text-sm font-medium flex items-center gap-2">
              <Terminal className="h-4 w-4" />
              Command
            </CardTitle>
            {!running && (
              <Badge variant="warning">
                sandbox {sandbox.status}
              </Badge>
            )}
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          {/* Preset chips — zero-friction common probes */}
          <div className="flex flex-wrap gap-1.5">
            {PRESET_CMDS.map((p) => (
              <Button
                key={p.cmd}
                variant="outline"
                size="sm"
                className="h-7 text-xs font-mono"
                disabled={!running || execMutationHook.isPending}
                onClick={() => {
                  setCmdInput(p.cmd)
                  runExec({ cmd: p.cmd, shell: p.shell })
                }}
                title={p.hint}
              >
                {p.label}
              </Button>
            ))}
          </div>

          <div className="flex flex-col gap-2 sm:flex-row">
            <div className="min-w-0 flex-1 flex items-center gap-2 rounded-md border bg-muted/30 px-2">
              <span className="font-mono text-xs text-muted-foreground select-none shrink-0">
                $
              </span>
              <Input
                ref={cmdInputRef}
                value={cmdInput}
                placeholder='ls -la  (use "quotes" to group)'
                onChange={(e) => setCmdInput(e.target.value)}
                onKeyDown={(e) => {
                  if (
                    e.key === 'Enter' &&
                    !execMutationHook.isPending &&
                    cmdInput.trim().length > 0 &&
                    running
                  ) {
                    runExec()
                  }
                }}
                className="min-w-0 border-0 bg-transparent shadow-none focus-visible:ring-0 px-0 font-mono h-9 text-base sm:text-sm"
              />
            </div>
            <div className="flex gap-2">
              <Input
                value={cwdInput}
                placeholder={sandbox.work_dir}
                onChange={(e) => setCwdInput(e.target.value)}
                className="font-mono h-9 w-full sm:w-56 text-base sm:text-xs"
                title="Working directory (optional)"
              />
              <Button
                onClick={() => runExec()}
                disabled={
                  execMutationHook.isPending ||
                  cmdInput.trim().length === 0 ||
                  !running
                }
                className="gap-1 shrink-0"
              >
                {execMutationHook.isPending ? (
                  <>
                    <Loader2 className="h-4 w-4 animate-spin" />
                    Running…
                  </>
                ) : (
                  <>Run</>
                )}
              </Button>
            </div>
          </div>

          {execResult && (
            <div className="rounded-md border bg-background">
              <div className="flex items-center justify-between px-3 py-2 border-b bg-muted/40">
                <div className="flex items-center gap-2 text-xs min-w-0">
                  <Badge
                    variant={
                      execResult.exit_code === 0 ? 'success' : 'destructive'
                    }
                  >
                    exit {execResult.exit_code}
                  </Badge>
                  {execDuration !== null && (
                    <span className="text-muted-foreground">
                      {execDuration}ms
                    </span>
                  )}
                  {execStartedCmd && (
                    <code className="font-mono text-muted-foreground truncate">
                      $ {execStartedCmd}
                    </code>
                  )}
                </div>
                <CopyButton
                  value={
                    execResult.stdout +
                    (execResult.stderr ? `\n---stderr---\n${execResult.stderr}` : '')
                  }
                  minimal
                  className="h-7 w-7 shrink-0"
                />
              </div>
              <div className="p-3 space-y-3">
                <OutputBlock label="stdout" text={execResult.stdout} />
                <OutputBlock label="stderr" text={execResult.stderr} error />
              </div>
            </div>
          )}
        </CardContent>
      </Card>

      {/* Detached jobs. Self-hides when the sandbox has none so it
          doesn't waste space on the common case. Shows cmd, status, age,
          and exit code at a glance; the full stdout/stderr lives behind
          `GET /jobs/{id}` which we wire to a drilldown later. */}
      {jobs.length > 0 && (
        <Card>
          <CardHeader className="pb-3 flex flex-row items-center justify-between">
            <CardTitle className="text-sm font-medium">
              Background jobs{' '}
              <span className="text-muted-foreground font-normal">
                · {jobs.length}
              </span>
            </CardTitle>
          </CardHeader>
          <CardContent className="p-0">
            <ul className="divide-y">
              {jobs.map((j) => (
                <JobRow key={j.id} sandboxId={sandboxId} job={j} now={now} />
              ))}
            </ul>
          </CardContent>
        </Card>
      )}

      {/* Secondary: sandbox identity — collapsed, low-visual-weight */}
      <div className="grid gap-4 grid-cols-1 md:grid-cols-2">
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-sm font-medium">Identity</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2 text-sm">
            <InfoRow label="Name" value={sandbox.name} />
            <InfoRow
              label="Image"
              value={sandbox.image ?? 'platform default'}
              mono
            />
            {sandbox.backend === 'firecracker' && (
              <div className="flex items-start justify-between gap-4">
                <span className="text-muted-foreground shrink-0">Disk</span>
                <div className="flex items-center gap-2">
                  <span className="text-right">
                    {sandbox.disk_size_mb
                      ? sandbox.disk_size_mb >= 1024
                        ? `${(sandbox.disk_size_mb / 1024).toFixed(sandbox.disk_size_mb % 1024 ? 1 : 0)} GiB`
                        : `${sandbox.disk_size_mb} MiB`
                      : '1 GiB (default)'}
                  </span>
                  {!destroyed && (
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-6 px-2 text-xs"
                      disabled={resizeMutation.isPending}
                      onClick={() => setResizeOpen(true)}
                    >
                      {resizeMutation.isPending ? (
                        <Loader2 className="h-3 w-3 animate-spin" />
                      ) : (
                        'Resize'
                      )}
                    </Button>
                  )}
                </div>
              </div>
            )}
            <InfoRow label="Work dir" value={sandbox.work_dir} mono />
          </CardContent>
        </Card>

        {hasPreview && (
          <Card>
            <CardHeader className="pb-3">
              <div className="flex items-center justify-between gap-2">
                <CardTitle className="text-sm font-medium">
                  Preview URL template
                </CardTitle>
                <CopyButton
                  value={sandbox.preview_url_template}
                  minimal
                  className="h-6 w-6"
                />
              </div>
            </CardHeader>
            <CardContent className="text-sm">
              <code className="block font-mono text-xs bg-muted px-2 py-1.5 rounded break-all">
                {sandbox.preview_url_template}
              </code>
              <p className="text-xs text-muted-foreground mt-2">
                Substitute <code className="font-mono">{'{port}'}</code> for
                any port bound inside the sandbox. Use "Open preview" above
                to launch common dev-server ports directly.
              </p>
            </CardContent>
          </Card>
        )}
      </div>

      {/* Operations timeline — lifecycle events only, never shell activity. */}
      {events.length > 0 && (
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-sm font-medium">Timeline</CardTitle>
          </CardHeader>
          <CardContent>
            <ol className="space-y-3">
              {events.map((e, i) => {
                const { icon: Icon, label, detail } = eventPresentation(e)
                return (
                  <li
                    key={`${e.at}-${e.event_type}-${i}`}
                    className="flex items-start gap-3"
                  >
                    <span className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full border bg-muted/40">
                      <Icon className="h-3.5 w-3.5 text-muted-foreground" />
                    </span>
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-baseline gap-x-2">
                        <span className="text-sm font-medium">{label}</span>
                        {detail && (
                          <span className="font-mono text-xs text-muted-foreground">
                            {detail}
                          </span>
                        )}
                      </div>
                      <div
                        className="text-xs text-muted-foreground"
                        title={new Date(e.at).toLocaleString()}
                      >
                        {formatAge(new Date(e.at).toISOString(), now)}
                      </div>
                    </div>
                  </li>
                )
              })}
            </ol>
          </CardContent>
        </Card>
      )}

      {/* Preview access gate — only meaningful when preview URLs are
          configured on this install. Destroyed sandboxes freeze the
          controls since mutating a dead row is never useful. */}
      {Boolean(sandbox.preview_url_template) && (
        <SandboxPreviewPasswordCard
          sandboxId={sandbox.id}
          hint={sandbox.preview_password_hint ?? undefined}
          disabled={destroyed}
        />
      )}

      {/* Resize dialog */}
      <AlertDialog open={resizeOpen} onOpenChange={setResizeOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Resize root disk</AlertDialogTitle>
            <AlertDialogDescription>
              Grow this sandbox's root disk. The disk can only grow, and the
              sandbox restarts briefly to apply it — the filesystem and its
              contents are preserved.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <div className="space-y-1.5">
            <Label htmlFor="resize-mb" className="text-xs text-muted-foreground">
              New size (MB) — current {sandbox.disk_size_mb ?? 1024} MB
            </Label>
            <Input
              id="resize-mb"
              type="number"
              min={(sandbox.disk_size_mb ?? 1024) + 1}
              placeholder={`> ${sandbox.disk_size_mb ?? 1024}`}
              value={resizeInput}
              onChange={(e) => setResizeInput(e.target.value)}
              className="font-mono"
            />
          </div>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              disabled={
                resizeMutation.isPending ||
                !resizeInput ||
                Number(resizeInput) <= (sandbox.disk_size_mb ?? 1024)
              }
              onClick={(e) => {
                e.preventDefault()
                resizeMutation.mutate({
                  path: { id: sandboxId },
                  body: { disk_size_mb: Number(resizeInput) },
                })
              }}
            >
              {resizeMutation.isPending ? 'Resizing…' : 'Resize'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Delete dialog */}
      <AlertDialog open={deleteOpen} onOpenChange={setDeleteOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete sandbox?</AlertDialogTitle>
            <AlertDialogDescription>
              This tears down the container for{' '}
              <span className="font-mono">{sandbox.id}</span>. The row is
              kept for audit but cannot be restarted.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() =>
                deleteMutation.mutate({ path: { id: sandboxId } })
              }
              disabled={deleteMutation.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {deleteMutation.isPending ? 'Deleting…' : 'Delete'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

// Map a JobSummary status string to the Badge variant that best signals
// the user-facing semantic: running = live (success-ish), exited with 0 =
// done, exited non-zero = warning (something returned an error code), and
// failed = destructive (the task itself errored, not the underlying cmd).
function jobStatusVariant(
  job: JobSummary,
): 'default' | 'secondary' | 'success' | 'warning' | 'destructive' | 'outline' {
  if (job.status === 'running') return 'secondary'
  if (job.status === 'failed') return 'destructive'
  if (job.status === 'exited') {
    return job.exit_code === 0 ? 'success' : 'warning'
  }
  return 'outline'
}

function jobStatusLabel(job: JobSummary): string {
  if (job.status === 'running') return 'running'
  if (job.status === 'failed') return 'failed'
  if (job.status === 'exited') {
    return job.exit_code === 0 ? 'done' : `exit ${job.exit_code}`
  }
  return job.status
}

function JobRow({
  sandboxId,
  job,
  now,
}: {
  sandboxId: string
  job: JobSummary
  now: number
}) {
  const [open, setOpen] = useState(false)

  return (
    <li className="flex flex-col">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex items-center justify-between gap-3 px-4 py-2.5 hover:bg-muted/40 text-left"
      >
        <div className="min-w-0 flex-1 flex items-center gap-3">
          {open ? (
            <ChevronDown className="h-4 w-4 text-muted-foreground shrink-0" />
          ) : (
            <ChevronRight className="h-4 w-4 text-muted-foreground shrink-0" />
          )}
          <Badge variant={jobStatusVariant(job)} className="shrink-0">
            {jobStatusLabel(job)}
          </Badge>
          <code
            className="truncate text-xs font-mono text-foreground"
            title={job.cmd}
          >
            {job.cmd || <span className="text-muted-foreground">(no cmd)</span>}
          </code>
        </div>
        <div className="flex items-center gap-3 shrink-0 text-xs text-muted-foreground">
          <span title={formatDate(job.started_at)}>
            {formatAge(job.started_at, now)}
          </span>
          <code className="font-mono hidden sm:inline">{job.id}</code>
        </div>
      </button>
      {open && (
        <JobLogsPanel sandboxId={sandboxId} job={job} />
      )}
    </li>
  )
}

/**
 * Live log viewer for one detached job. On mount: fetches the history
 * snapshot (so we don't miss lines produced before subscribing) and
 * opens an SSE connection for the live tail. The SSE connection is
 * closed automatically when the job is no longer running — the backend
 * emits a `done` event when the exec task exits, and it also closes if
 * the row collapses (component unmounts).
 *
 * Rendered separately so mount/unmount is tied to the collapsible open
 * state — a collapsed row holds no EventSource, which matters when a
 * sandbox has many running jobs.
 */
function JobLogsPanel({
  sandboxId,
  job,
}: {
  sandboxId: string
  job: JobSummary
}) {
  const [history, setHistory] = useState<{
    stdout: string
    stderr: string
  } | null>(null)
  const [live, setLive] = useState<string>('')
  const [error, setError] = useState<string | null>(null)
  const [connected, setConnected] = useState(false)
  const scrollRef = useRef<HTMLPreElement>(null)

  useEffect(() => {
    let cancelled = false
    jobStatus({ path: { id: sandboxId, job_id: job.id } })
      .then((res) => {
        if (cancelled) return
        const s = res.data
        if (!s) {
          setError('No job data')
          return
        }
        setHistory({ stdout: s.stdout, stderr: s.stderr })
      })
      .catch((e: Error) => {
        if (!cancelled) setError(e.message)
      })
    return () => {
      cancelled = true
    }
  }, [sandboxId, job.id])

  useEffect(() => {
    // Don't bother opening an SSE for terminal jobs — the history
    // snapshot already has everything.
    if (job.status !== 'running') return
    const es = new EventSource(jobLogsUrl(sandboxId, job.id))
    es.addEventListener('log', (ev) => {
      try {
        const parsed = JSON.parse((ev as MessageEvent).data) as {
          stream: string
          data: string
        }
        // Prefix stderr with `!` so it's distinguishable in the scrollback
        // without needing a second pane. Kept light — a human looking at
        // the feed cares about interleaved order far more than color.
        //
        // The backend strips trailing newlines before emitting
        // (see docker.rs exec loop — it iterates `.lines()` and sends
        // each stripped line). Re-add `\n` here so the <pre> renders
        // them as separate rows instead of a wall of text.
        const prefix = parsed.stream === 'stderr' ? '! ' : ''
        const line = parsed.data.endsWith('\n')
          ? parsed.data
          : parsed.data + '\n'
        setLive((prev) => prev + prefix + line)
      } catch {
        /* ignore malformed event */
      }
    })
    es.addEventListener('done', () => {
      es.close()
      setConnected(false)
    })
    es.onopen = () => setConnected(true)
    es.onerror = () => {
      setConnected(false)
      es.close()
    }
    return () => {
      es.close()
      setConnected(false)
    }
  }, [sandboxId, job.id, job.status])

  useEffect(() => {
    // Auto-scroll to bottom as new lines arrive. Users who scroll up
    // to inspect history still get it — we only pin to bottom on each
    // update, which feels like a tail. A "pause scroll" toggle is a
    // future refinement if people complain.
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight
    }
  }, [live, history])

  const stdoutText = (history?.stdout ?? '') + live
  const stderrText = history?.stderr ?? ''

  return (
    <div className="px-4 pb-3 pt-1 space-y-2 bg-muted/20 border-t">
      <div className="flex items-center justify-between text-xs text-muted-foreground">
        <div className="flex items-center gap-3">
          <code className="font-mono">{job.id}</code>
          <CopyButton value={job.id} className="h-6 w-6" />
          {job.status === 'running' && connected && (
            <span className="flex items-center gap-1">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-500 animate-pulse" />
              live
            </span>
          )}
        </div>
        {error && <span className="text-destructive">{error}</span>}
      </div>
      <pre
        ref={scrollRef}
        className="max-h-80 overflow-auto rounded-md border bg-background p-2 font-mono text-xs leading-relaxed"
      >
        {stdoutText || (
          <span className="text-muted-foreground italic">
            {history ? '(no stdout yet)' : 'loading…'}
          </span>
        )}
      </pre>
      {stderrText.trim().length > 0 && (
        <div className="space-y-1">
          <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
            stderr
          </div>
          <pre className="max-h-40 overflow-auto rounded-md border bg-background p-2 font-mono text-xs leading-relaxed text-destructive/90">
            {stderrText}
          </pre>
        </div>
      )}
    </div>
  )
}

function InfoRow({
  label,
  value,
  mono,
}: {
  label: string
  value: React.ReactNode
  mono?: boolean
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <span className="text-muted-foreground shrink-0">{label}</span>
      <span
        className={`text-right break-all ${mono ? 'font-mono text-xs' : ''}`}
      >
        {value}
      </span>
    </div>
  )
}

function OutputBlock({
  label,
  text,
  error,
}: {
  label: string
  text: string
  error?: boolean
}) {
  if (text.length === 0) {
    return (
      <div className="space-y-1">
        <div className="text-xs font-medium text-muted-foreground uppercase tracking-wide">
          {label}
        </div>
        <div className="text-xs text-muted-foreground italic">(empty)</div>
      </div>
    )
  }
  return (
    <div className="space-y-1">
      <div className="text-xs font-medium text-muted-foreground uppercase tracking-wide">
        {label}
      </div>
      <pre
        className={`text-xs font-mono whitespace-pre-wrap break-all rounded border p-3 max-h-72 overflow-auto ${
          error ? 'bg-destructive/5 border-destructive/30' : 'bg-muted/40'
        }`}
      >
        {text}
      </pre>
    </div>
  )
}
