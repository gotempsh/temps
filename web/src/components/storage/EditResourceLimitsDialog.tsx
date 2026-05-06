import {
  getServiceRuntimeQueryKey,
  getServiceStatsQueryKey,
  updateServiceResourcesMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ResourceLimitsUpdateResponse,
  ServiceResourceLimits,
} from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
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
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, Info, Loader2 } from 'lucide-react'
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
  // Each cap is independently toggleable. Memory and swap travel together
  // (Docker requires swap >= memory when both are set; we make swap
  // implicit and equal to memory by default = "no swap"). We surface a
  // separate switch for "allow swap" so power users can opt in.
  const [memoryEnabled, setMemoryEnabled] = useState(false)
  const [memoryMb, setMemoryMb] = useState<string>(String(DEFAULT_MEMORY_MB))
  const [swapEnabled, setSwapEnabled] = useState(false)
  const [swapMb, setSwapMb] = useState<string>(String(DEFAULT_MEMORY_MB))
  const [cpuEnabled, setCpuEnabled] = useState(false)
  const [cpuCores, setCpuCores] = useState<string>(String(DEFAULT_CPU_CORES))

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

    // Treat swap = memory as "swap disabled, not user-set" so we only
    // turn the swap switch on when the operator has explicitly raised it
    // above the memory cap.
    const userSetSwap = swap != null && memory != null && swap > memory
    setSwapEnabled(userSetSwap)
    setSwapMb(userSetSwap ? String(swap) : String(memory ?? DEFAULT_MEMORY_MB))

    setCpuEnabled(nano != null)
    setCpuCores(
      nano != null
        ? String(nanoCpusToCores(nano))
        : String(DEFAULT_CPU_CORES),
    )
  }, [open, currentLimits])

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
        if (s < m) {
          return 'Swap (memory + swap) must be ≥ memory limit.'
        }
      }
    }
    if (cpuEnabled) {
      const c = Number(cpuCores)
      if (!Number.isFinite(c) || c <= 0) {
        return 'CPU cores must be greater than zero.'
      }
    }
    return null
  }, [memoryEnabled, memoryMb, swapEnabled, swapMb, cpuEnabled, cpuCores])

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
            'Removing a memory cap on a running container needs a recreate. Restart the service to apply.',
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
    const body: ServiceResourceLimits = {
      memory_mb: memoryEnabled ? Math.round(Number(memoryMb)) : null,
      // When memory is on but swap isn't explicitly raised, set swap = memory
      // so swap is fully disabled (Docker semantics: memory_swap == memory
      // means "no swap"). Without this, kernel default swap remains in play
      // and the cap has soft edges.
      memory_swap_mb: memoryEnabled
        ? swapEnabled
          ? Math.round(Number(swapMb))
          : Math.round(Number(memoryMb))
        : null,
      nano_cpus: cpuEnabled ? coresToNanoCpus(Number(cpuCores)) : null,
      cpu_shares: null,
    }
    mutation.mutate({ path: { id: serviceId }, body })
  }

  const allUnlimited = !memoryEnabled && !cpuEnabled

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Resource limits — {serviceName}</DialogTitle>
          <DialogDescription>
            Cap memory and CPU available to this service's container.
            Changes apply live to running containers without a restart.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-5">
          {/* OOM warning whenever a memory cap is on. Operators frequently
              enable hard limits without realizing the kernel kills the
              container outright when the working set exceeds them. */}
          {memoryEnabled ? (
            <Alert variant="destructive">
              <AlertTriangle className="h-4 w-4" />
              <AlertTitle>OOM kills are possible</AlertTitle>
              <AlertDescription>
                If the container exceeds {memoryMb || '0'} MiB the kernel
                will terminate it. The container restarts automatically, but
                in-flight queries fail. Watch the runtime panel's
                <span className="font-medium"> restart count </span>
                after enabling.
              </AlertDescription>
            </Alert>
          ) : (
            <Alert>
              <Info className="h-4 w-4" />
              <AlertDescription>
                Leave both switches off to run unconstrained — same as
                today. Enable a cap only if a runaway workload has already
                degraded the host.
              </AlertDescription>
            </Alert>
          )}

          {/* Memory ---------------------------------------------------- */}
          <div className="space-y-3 rounded-md border p-4">
            <div className="flex items-center justify-between gap-4">
              <div>
                <Label htmlFor="resource-memory-toggle" className="text-base">
                  Memory limit
                </Label>
                <p className="text-sm text-muted-foreground">
                  Hard cap on resident set. Container is OOM-killed past
                  this.
                </p>
              </div>
              <Switch
                id="resource-memory-toggle"
                checked={memoryEnabled}
                onCheckedChange={setMemoryEnabled}
              />
            </div>
            {memoryEnabled ? (
              <div className="space-y-3 pt-2">
                <div className="grid grid-cols-[1fr_auto] items-center gap-3">
                  <Input
                    id="resource-memory-mb"
                    type="number"
                    min={1}
                    step={64}
                    value={memoryMb}
                    onChange={(e) => setMemoryMb(e.target.value)}
                  />
                  <span className="text-sm text-muted-foreground">MiB</span>
                </div>

                <div className="flex items-start justify-between gap-4 border-t pt-3">
                  <div>
                    <Label
                      htmlFor="resource-swap-toggle"
                      className="text-sm"
                    >
                      Allow swap
                    </Label>
                    <p className="text-xs text-muted-foreground">
                      Off (default): swap disabled. On: lets the container
                      page out up to swap-MiB above the memory cap.
                    </p>
                  </div>
                  <Switch
                    id="resource-swap-toggle"
                    checked={swapEnabled}
                    onCheckedChange={setSwapEnabled}
                  />
                </div>
                {swapEnabled ? (
                  <div className="grid grid-cols-[1fr_auto] items-center gap-3">
                    <Input
                      id="resource-swap-mb"
                      type="number"
                      min={1}
                      step={64}
                      value={swapMb}
                      onChange={(e) => setSwapMb(e.target.value)}
                    />
                    <span className="text-sm text-muted-foreground">
                      MiB total
                    </span>
                  </div>
                ) : null}
              </div>
            ) : null}
          </div>

          {/* CPU ------------------------------------------------------- */}
          <div className="space-y-3 rounded-md border p-4">
            <div className="flex items-center justify-between gap-4">
              <div>
                <Label htmlFor="resource-cpu-toggle" className="text-base">
                  CPU limit
                </Label>
                <p className="text-sm text-muted-foreground">
                  Cap CPU at N cores (fractional allowed, e.g. 0.5).
                </p>
              </div>
              <Switch
                id="resource-cpu-toggle"
                checked={cpuEnabled}
                onCheckedChange={setCpuEnabled}
              />
            </div>
            {cpuEnabled ? (
              <div className="grid grid-cols-[1fr_auto] items-center gap-3 pt-2">
                <Input
                  id="resource-cpu-cores"
                  type="number"
                  min={0.1}
                  step={0.1}
                  value={cpuCores}
                  onChange={(e) => setCpuCores(e.target.value)}
                />
                <span className="text-sm text-muted-foreground">cores</span>
              </div>
            ) : null}
          </div>

          {validation ? (
            <Alert variant="destructive">
              <AlertTriangle className="h-4 w-4" />
              <AlertDescription>{validation}</AlertDescription>
            </Alert>
          ) : null}
        </div>

        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={mutation.isPending}
          >
            Cancel
          </Button>
          <Button
            onClick={handleSubmit}
            disabled={mutation.isPending || validation != null}
          >
            {mutation.isPending ? (
              <>
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                Saving…
              </>
            ) : allUnlimited ? (
              'Save (unlimited)'
            ) : (
              'Save limits'
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
