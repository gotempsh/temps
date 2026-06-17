import {
  getServiceRuntimeQueryKey,
  getServiceStatsQueryKey,
  updateServiceResourcesMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ResourceLimitsUpdateResponse,
  ServiceResourceLimits,
} from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { cn } from '@/lib/utils'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, Loader2 } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'

interface EditResourceLimitsDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  serviceId: number
  serviceName: string
  /**
   * Currently-applied limits (read off the running container, not the
   * stored config). Used to seed the form. Null/undefined => unlimited.
   */
  currentLimits: ServiceResourceLimits | null | undefined
}

/**
 * Default per-CPU value used when the user toggles CPU limits on. Set to
 * one full core (1.0 CPU = 1_000_000_000 nano_cpus) — the most common
 * "I want a sane default" choice. The user can dial up/down from there.
 */
const DEFAULT_CPU_CORES = 1.0

/**
 * Sensible memory default when the user toggles memory limits on. Picked
 * to be small enough that operators consciously raise it (rather than
 * leave a too-generous default in place that wouldn't actually constrain
 * anything) but large enough that postgres/redis/mongo boot cleanly.
 */
const DEFAULT_MEMORY_MB = 512

/**
 * Docker's default /dev/shm is 64 MiB. We seed the input with that so the
 * displayed value matches what the container already has before the operator
 * raises it (Postgres parallel queries commonly need 256+ MiB).
 */
const DEFAULT_SHM_MB = 64

/**
 * Convert nano_cpus <-> fractional CPU cores. We expose cores in the UI
 * because "0.5 CPU" reads better than "500000000 nano_cpus" — but the
 * wire format requires nano_cpus (Docker's native units).
 */
function nanoCpusToCores(nano: number | null | undefined): number {
  if (nano == null || nano <= 0) return 0
  return nano / 1_000_000_000
}

function coresToNanoCpus(cores: number): number {
  return Math.max(1, Math.round(cores * 1_000_000_000))
}

export function EditResourceLimitsDialog({
  open,
  onOpenChange,
  serviceId,
  serviceName,
  currentLimits,
}: EditResourceLimitsDialogProps) {
  // -- Form state ---------------------------------------------------------
  // Each cap is independently toggleable. Docker's wire format wants
  // `memory_swap` as the *total* (memory + swap); we hide that footgun by
  // having the user enter additional swap MiB and adding memory at submit
  // time. `swapMb` here is therefore "extra swap on top of the memory cap".
  const [memoryEnabled, setMemoryEnabled] = useState(false)
  const [memoryMb, setMemoryMb] = useState<string>(String(DEFAULT_MEMORY_MB))
  const [swapEnabled, setSwapEnabled] = useState(false)
  const [swapMb, setSwapMb] = useState<string>(String(DEFAULT_MEMORY_MB))
  const [cpuEnabled, setCpuEnabled] = useState(false)
  const [cpuCores, setCpuCores] = useState<string>(String(DEFAULT_CPU_CORES))
  // Shared memory (/dev/shm). Docker defaults to 64 MiB; unlike memory/CPU
  // this is create-time-only, so changing it recreates the container.
  const [shmEnabled, setShmEnabled] = useState(false)
  const [shmMb, setShmMb] = useState<string>(String(DEFAULT_SHM_MB))

  // Re-seed the form whenever the dialog opens (or current limits arrive).
  // Without this the user sees stale state from the previous service when
  // they switch services and re-open the dialog.
  useEffect(() => {
    if (!open) return
    const memory = currentLimits?.memory_mb ?? null
    const swap = currentLimits?.memory_swap_mb ?? null
    const nano = currentLimits?.nano_cpus ?? null

    setMemoryEnabled(memory != null)
    setMemoryMb(memory != null ? String(memory) : String(DEFAULT_MEMORY_MB))

    // The stored `swap` value is Docker's `memory_swap` total (memory + swap).
    // Convert back to "extra swap" for the form so the input matches the
    // label. swap == memory means no swap; swap > memory means the operator
    // explicitly added some.
    const extraSwap =
      swap != null && memory != null && swap > memory ? swap - memory : 0
    setSwapEnabled(extraSwap > 0)
    setSwapMb(
      extraSwap > 0 ? String(extraSwap) : String(memory ?? DEFAULT_MEMORY_MB),
    )

    setCpuEnabled(nano != null)
    setCpuCores(
      nano != null
        ? String(nanoCpusToCores(nano))
        : String(DEFAULT_CPU_CORES),
    )

    const shm = currentLimits?.shm_size_mb ?? null
    setShmEnabled(shm != null)
    setShmMb(shm != null ? String(shm) : String(DEFAULT_SHM_MB))
  }, [open, currentLimits])

  // Reset input to a sane default when toggling a limit back on, so a
  // previously typed invalid value (e.g. "-1") doesn't reappear.
  const handleMemoryToggle = (v: boolean) => {
    if (v && (Number(memoryMb) <= 0 || !Number.isFinite(Number(memoryMb)))) {
      setMemoryMb(String(DEFAULT_MEMORY_MB))
    }
    setMemoryEnabled(v)
  }
  const handleCpuToggle = (v: boolean) => {
    if (v && (Number(cpuCores) <= 0 || !Number.isFinite(Number(cpuCores)))) {
      setCpuCores(String(DEFAULT_CPU_CORES))
    }
    setCpuEnabled(v)
  }
  const handleShmToggle = (v: boolean) => {
    if (v && (Number(shmMb) <= 0 || !Number.isFinite(Number(shmMb)))) {
      setShmMb(String(DEFAULT_SHM_MB))
    }
    setShmEnabled(v)
  }

  // -- Validation ---------------------------------------------------------
  const validation = useMemo(() => {
    if (memoryEnabled) {
      const m = Number(memoryMb)
      if (!Number.isFinite(m) || m <= 0) {
        return 'Memory must be a positive number of MiB.'
      }
      if (swapEnabled) {
        const s = Number(swapMb)
        if (!Number.isFinite(s) || s <= 0) {
          return 'Swap must be a positive number of MiB.'
        }
      }
    }
    if (cpuEnabled) {
      const c = Number(cpuCores)
      if (!Number.isFinite(c) || c <= 0) {
        return 'CPU cores must be greater than zero.'
      }
    }
    if (shmEnabled) {
      const s = Number(shmMb)
      if (!Number.isFinite(s) || s <= 0) {
        return 'Shared memory must be a positive number of MiB.'
      }
    }
    return null
  }, [
    memoryEnabled,
    memoryMb,
    swapEnabled,
    swapMb,
    cpuEnabled,
    cpuCores,
    shmEnabled,
    shmMb,
  ])

  // -- Mutation -----------------------------------------------------------
  const queryClient = useQueryClient()
  const mutation = useMutation({
    ...updateServiceResourcesMutation(),
    onSuccess: (response: ResourceLimitsUpdateResponse) => {
      // Build a single summary toast that reflects what actually happened
      // per member. "applied" = live; "stopped" = stored, takes effect on
      // next start; "missing" = container doesn't exist yet; "failed" = the
      // Docker daemon rejected the update (typically: new memory cap is
      // below current usage).
      const members = response.applied ?? []
      const live = members.filter((m) => m.outcome === 'applied').length
      const stopped = members.filter((m) => m.outcome === 'stopped').length
      const missing = members.filter((m) => m.outcome === 'missing').length
      const failed = members.filter((m) => m.outcome === 'failed')
      // Docker can't remove a memory cap on a running container — when
      // the operator switches from limited → unlimited, the live update
      // is a no-op and only a container recreate picks up the change.
      const recreate = members.filter((m) => m.outcome === 'requires_recreate')

      if (failed.length > 0) {
        toast.warning('Limits saved, but some containers rejected the update', {
          description: failed
            .map((f) => `${f.container_name}: ${f.error ?? 'failed'}`)
            .join('\n'),
        })
      } else if (recreate.length > 0) {
        toast.warning('Limits saved — restart required', {
          description:
            'Some changes (shared memory, or removing a memory cap) can\'t be applied live. Restart the service to recreate the container and apply them.',
        })
      } else if (members.length === 0) {
        toast.success('Resource limits saved', {
          description:
            'No running containers found — caps will apply on next start.',
        })
      } else {
        const parts = [
          live > 0 ? `${live} live` : null,
          stopped > 0 ? `${stopped} on next start` : null,
          missing > 0 ? `${missing} not yet created` : null,
        ].filter(Boolean) as string[]
        toast.success('Resource limits applied', {
          description: parts.join(' · '),
        })
      }

      // Refresh runtime + stats panels so the new caps and any
      // OOM/restart fallout show up immediately.
      queryClient.invalidateQueries({
        queryKey: getServiceRuntimeQueryKey({ path: { id: serviceId } }),
      })
      queryClient.invalidateQueries({
        queryKey: getServiceStatsQueryKey({ path: { id: serviceId } }),
      })
      onOpenChange(false)
    },
    onError: (err) => {
      const message =
        err instanceof Error ? err.message : 'Unknown error updating limits.'
      toast.error('Failed to update resource limits', { description: message })
    },
  })

  // -- Submit -------------------------------------------------------------
  const handleSubmit = () => {
    if (validation) {
      toast.error(validation)
      return
    }
    const memoryRounded = memoryEnabled ? Math.round(Number(memoryMb)) : null
    const extraSwap =
      memoryEnabled && swapEnabled ? Math.round(Number(swapMb)) : 0
    const body: ServiceResourceLimits = {
      memory_mb: memoryRounded,
      // Docker's `memory_swap` is the *total* (memory + swap). We compute
      // it here so the user only enters the extra-swap amount they want.
      // When swap is off, total == memory == "swap fully disabled" (Docker:
      // memory_swap == memory means no swap).
      memory_swap_mb:
        memoryRounded != null ? memoryRounded + extraSwap : null,
      nano_cpus: cpuEnabled ? coresToNanoCpus(Number(cpuCores)) : null,
      cpu_shares: null,
      // Create-time-only: the backend recreates the container to apply a
      // changed /dev/shm size (Docker's live update API can't touch it).
      shm_size_mb: shmEnabled ? Math.round(Number(shmMb)) : null,
    }
    mutation.mutate({ path: { id: serviceId }, body })
  }

  // -- Diff vs. current limits (drives "no changes" disabled state) -----
  const isDirty = useMemo(() => {
    const currentMem = currentLimits?.memory_mb ?? null
    const currentSwap = currentLimits?.memory_swap_mb ?? null
    const currentNano = currentLimits?.nano_cpus ?? null
    const newMem = memoryEnabled ? Math.round(Number(memoryMb)) || null : null
    const newSwapTotal =
      newMem != null
        ? newMem +
          (swapEnabled ? Math.round(Number(swapMb)) || 0 : 0)
        : null
    const newNano = cpuEnabled ? coresToNanoCpus(Number(cpuCores)) : null
    const currentShm = currentLimits?.shm_size_mb ?? null
    const newShm = shmEnabled ? Math.round(Number(shmMb)) || null : null
    return (
      newMem !== currentMem ||
      newSwapTotal !== currentSwap ||
      newNano !== currentNano ||
      newShm !== currentShm
    )
  }, [
    currentLimits,
    memoryEnabled,
    memoryMb,
    swapEnabled,
    swapMb,
    cpuEnabled,
    cpuCores,
    shmEnabled,
    shmMb,
  ])

  // Pretty-print the currently applied value as a small chip on each row.
  const currentMemoryLabel =
    currentLimits?.memory_mb != null
      ? `${currentLimits.memory_mb} MiB`
      : 'Unlimited'
  const currentCpuLabel =
    currentLimits?.nano_cpus != null
      ? `${formatCores(nanoCpusToCores(currentLimits.nano_cpus))} cores`
      : 'Unlimited'
  // /dev/shm has a real Docker default (64 MiB) rather than "unlimited", so
  // show that when the operator hasn't set an explicit size.
  const currentShmLabel =
    currentLimits?.shm_size_mb != null
      ? `${currentLimits.shm_size_mb} MiB`
      : `${DEFAULT_SHM_MB} MiB (default)`
  const currentSwapExtra =
    currentLimits?.memory_mb != null &&
    currentLimits?.memory_swap_mb != null &&
    currentLimits.memory_swap_mb > currentLimits.memory_mb
      ? currentLimits.memory_swap_mb - currentLimits.memory_mb
      : 0

  const memoryTotal = useMemo(() => {
    const m = Number(memoryMb)
    const s = swapEnabled ? Number(swapMb) : 0
    if (!Number.isFinite(m) || !Number.isFinite(s)) return null
    return Math.round(m + s)
  }, [memoryMb, swapEnabled, swapMb])

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="text-base">
            Resource limits
            <span className="ml-2 font-normal text-muted-foreground">
              {serviceName}
            </span>
          </DialogTitle>
          <DialogDescription className="text-xs">
            Memory and CPU apply live. Changing shared memory recreates the
            container.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-5">
          {/* Memory ---------------------------------------------------- */}
          <LimitSection
            label="Memory"
            currentLabel={currentMemoryLabel}
            enabled={memoryEnabled}
            onToggle={handleMemoryToggle}
            toggleId="resource-memory-toggle"
            description="Hard cap. Container is OOM-killed past this."
          >
            <div className="space-y-3">
              <PresetInput
                id="resource-memory-mb"
                value={memoryMb}
                onChange={setMemoryMb}
                presets={MEMORY_PRESETS}
                unit="MiB"
                min={64}
                step={64}
              />

              {/* Swap sub-row — only meaningful when memory is on, so it's
                  nested as part of the memory section. */}
              <div className="rounded-md border-l-2 border-muted pl-3">
                <div className="flex items-center justify-between gap-3">
                  <div>
                    <Label
                      htmlFor="resource-swap-toggle"
                      className="cursor-pointer text-sm"
                    >
                      Allow swap
                    </Label>
                    <p className="text-xs text-muted-foreground">
                      Page out N MiB beyond the memory cap.
                    </p>
                  </div>
                  <Switch
                    id="resource-swap-toggle"
                    checked={swapEnabled}
                    onCheckedChange={setSwapEnabled}
                  />
                </div>
                {swapEnabled && (
                  <div className="mt-3 space-y-2">
                    <PresetInput
                      id="resource-swap-mb"
                      value={swapMb}
                      onChange={setSwapMb}
                      presets={SWAP_PRESETS}
                      unit="MiB"
                      min={64}
                      step={64}
                    />
                    {memoryTotal != null && (
                      <p className="text-xs tabular-nums text-muted-foreground">
                        Total memory + swap{' '}
                        <span className="font-medium text-foreground">
                          {memoryTotal} MiB
                        </span>
                      </p>
                    )}
                  </div>
                )}
              </div>
            </div>
          </LimitSection>

          {/* CPU ------------------------------------------------------- */}
          <LimitSection
            label="CPU"
            currentLabel={currentCpuLabel}
            enabled={cpuEnabled}
            onToggle={handleCpuToggle}
            toggleId="resource-cpu-toggle"
            description="Cap at N cores. Fractional allowed."
          >
            <PresetInput
              id="resource-cpu-cores"
              value={cpuCores}
              onChange={setCpuCores}
              presets={CPU_PRESETS}
              unit="cores"
              min={0.1}
              step={0.1}
              decimal
            />
          </LimitSection>

          {/* Shared memory (/dev/shm) -------------------------------- */}
          <LimitSection
            label="Shared memory"
            currentLabel={currentShmLabel}
            enabled={shmEnabled}
            onToggle={handleShmToggle}
            toggleId="resource-shm-toggle"
            description={
              'Shared memory (/dev/shm). Postgres uses it for parallel queries; ' +
              'the 64 MB default can cause "No space left on device" errors. ' +
              'Changing this recreates the container (brief downtime).'
            }
          >
            <PresetInput
              id="resource-shm-mb"
              value={shmMb}
              onChange={setShmMb}
              presets={SHM_PRESETS}
              unit="MiB"
              min={64}
              step={64}
            />
          </LimitSection>

          {validation ? (
            <Alert variant="destructive">
              <AlertTriangle className="h-4 w-4" />
              <AlertDescription className="text-xs">
                {validation}
              </AlertDescription>
            </Alert>
          ) : null}
        </div>

        <DialogFooter className="gap-2 sm:gap-2">
          <div className="mr-auto text-xs text-muted-foreground">
            {currentSwapExtra > 0 && !swapEnabled && memoryEnabled && (
              <span>+{currentSwapExtra} MiB swap currently allowed</span>
            )}
          </div>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={mutation.isPending}
          >
            Cancel
          </Button>
          <Button
            type="button"
            onClick={handleSubmit}
            disabled={
              mutation.isPending || validation != null || !isDirty
            }
          >
            {mutation.isPending ? (
              <>
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                Saving…
              </>
            ) : !isDirty ? (
              'No changes'
            ) : (
              'Save'
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// Helpers and small components
// ---------------------------------------------------------------------------

const MEMORY_PRESETS = [256, 512, 1024, 2048, 4096]
const SWAP_PRESETS = [128, 256, 512, 1024]
const CPU_PRESETS = [0.25, 0.5, 1, 2, 4]
const SHM_PRESETS = [64, 128, 256, 512, 1024]

function formatCores(n: number): string {
  return n % 1 === 0 ? n.toString() : n.toFixed(2).replace(/\.?0+$/, '')
}

/**
 * One row per cap (Memory, CPU). Replaces the previous bordered card with
 * a flat label/toggle pair plus a "Currently: X" chip so the operator can
 * see the applied value without enabling the switch.
 */
function LimitSection({
  label,
  description,
  currentLabel,
  enabled,
  onToggle,
  toggleId,
  children,
}: {
  label: string
  description: string
  currentLabel: string
  enabled: boolean
  onToggle: (v: boolean) => void
  toggleId: string
  children: React.ReactNode
}) {
  const isCurrentlyLimited = !currentLabel.toLowerCase().includes('unlimited')
  return (
    <section className="space-y-3">
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-baseline gap-2">
            <Label
              htmlFor={toggleId}
              className="cursor-pointer text-sm font-medium"
            >
              {label}
            </Label>
            <span className="rounded-sm bg-muted px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-muted-foreground">
              now {currentLabel}
            </span>
          </div>
          <p className="mt-0.5 text-xs text-muted-foreground">
            {!enabled && isCurrentlyLimited
              ? 'Toggle on to change, or save now to remove the limit.'
              : description}
          </p>
        </div>
        <Switch
          id={toggleId}
          checked={enabled}
          onCheckedChange={onToggle}
        />
      </div>
      {enabled && <div className="pl-0">{children}</div>}
    </section>
  )
}

/**
 * Numeric input with a row of small "preset" buttons below. Clicking a
 * preset fills the input. Selected preset gets a subtle highlight so the
 * operator knows which value they're at.
 */
function PresetInput({
  id,
  value,
  onChange,
  presets,
  unit,
  min,
  step,
  decimal = false,
}: {
  id: string
  value: string
  onChange: (v: string) => void
  presets: number[]
  unit: string
  min: number
  step: number
  decimal?: boolean
}) {
  const numeric = Number(value)
  return (
    <div className="space-y-2">
      <div className="grid grid-cols-[1fr_auto] items-center gap-3">
        <Input
          id={id}
          type="number"
          inputMode={decimal ? 'decimal' : 'numeric'}
          min={min}
          step={step}
          value={value}
          onChange={(e) => onChange(e.target.value)}
        />
        <span className="text-sm text-muted-foreground">{unit}</span>
      </div>
      <div className="flex flex-wrap gap-1.5">
        {presets.map((preset) => {
          const isActive = Number.isFinite(numeric) && numeric === preset
          return (
            <button
              key={preset}
              type="button"
              onClick={() => onChange(String(preset))}
              className={cn(
                'rounded border px-2 py-0.5 text-xs tabular-nums transition-colors',
                isActive
                  ? 'border-foreground/40 bg-foreground/5 text-foreground'
                  : 'border-border text-muted-foreground hover:border-foreground/20 hover:text-foreground',
              )}
            >
              {decimal ? formatCores(preset) : preset}
            </button>
          )
        })}
      </div>
    </div>
  )
}
