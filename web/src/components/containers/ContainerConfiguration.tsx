import {
  ContainerDetailResponse,
  getContainerEnvironmentVariable,
} from '@/api/client'
import { CopyButton } from '@/components/ui/copy-button'
import { Button } from '@/components/ui/button'
import { formatMicrocores } from '@/lib/cpu-format'
import {
  createCredentialRevealGuard,
  credentialValueForScope,
  type ScopedCredentialValue,
} from '@/lib/credential-reveal-state'
import { Eye, EyeOff, Loader2 } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { toast } from 'sonner'

interface ContainerConfigurationProps {
  container: ContainerDetailResponse
  projectId: number
  environmentId: number
  containerId: string
}

export function ContainerConfiguration({
  container,
  projectId,
  environmentId,
  containerId,
}: ContainerConfigurationProps) {
  const envVars = normalizeEnvVars(container.environment_variables)
  const hasPorts = !!(container.container_port || container.host_port)
  // Configured limits (from project/env deployment_config). cpu_limit and
  // cpu_request are in MICROcores (1_000_000 = 1 core); memory in MB.
  const cpuLimitMicro = container.resource_limits?.cpu_limit
  const cpuRequestMicro = container.resource_limits?.cpu_request
  const memoryLimitMb = container.resource_limits?.memory_limit
  const memoryRequestMb = container.resource_limits?.memory_request
  // Actual CPU limit observed from Docker — useful for spotting drift between
  // configured value and what the container is actually running with.
  const cpuLimitObservedCores = (
    container as { cpu_limit_cores?: number | null }
  ).cpu_limit_cores
  const hasResourceLimits =
    cpuLimitMicro != null ||
    memoryLimitMb != null ||
    cpuRequestMicro != null ||
    memoryRequestMb != null ||
    cpuLimitObservedCores != null
  const startedAt = (container as { started_at?: string | null }).started_at
  const isExited =
    container.status === 'exited' ||
    container.status === 'dead' ||
    container.status === 'stopped'

  return (
    <div className="flex flex-col gap-10">
      <Section
        title="Basic information"
        description="Identity and runtime metadata for this container."
      >
        <FieldGrid>
          <Field
            label="Container ID"
            mono
            copyable
            value={container.container_id}
          />
          <Field label="Image" mono copyable value={container.image_name} />
          <Field label="Status" value={container.status} />
          <Field
            label="Uptime"
            // Prefer started_at — uptime should reset on a restart-in-place.
            // Fall back to created_at on older rows the migration hasn't
            // populated yet.
            value={formatUptimeFromTimestamp(startedAt || container.created_at)}
          />
          {container.service_name && (
            <Field label="Service" mono value={container.service_name} />
          )}
          {container.restart_count != null && container.restart_count > 0 && (
            <Field
              label="Restarts"
              value={String(container.restart_count)}
              tone="warn"
            />
          )}
        </FieldGrid>
      </Section>

      {hasResourceLimits && (
        <Section
          title="Resource limits"
          description="CPU and memory caps applied to this container at deploy time."
        >
          <FieldGrid>
            {cpuRequestMicro != null && (
              <Field
                label="CPU request"
                mono
                value={formatMicrocores(cpuRequestMicro)}
              />
            )}
            {cpuLimitMicro != null && (
              <Field
                label="CPU limit"
                mono
                value={formatMicrocores(cpuLimitMicro)}
              />
            )}
            {memoryRequestMb != null && (
              <Field
                label="Memory request"
                mono
                value={formatMemoryMb(memoryRequestMb)}
              />
            )}
            {memoryLimitMb != null && (
              <Field
                label="Memory limit"
                mono
                value={formatMemoryMb(memoryLimitMb)}
              />
            )}
            {cpuLimitObservedCores != null && (
              <Field
                label="CPU limit (observed)"
                mono
                value={formatCores(cpuLimitObservedCores)}
              />
            )}
          </FieldGrid>
        </Section>
      )}

      {isExited && container.exit_reason && (
        <Section
          title="Exit details"
          description="Why this container is no longer running."
        >
          <FieldGrid>
            <Field
              label="Reason"
              value={container.exit_reason}
              tone={container.oom_killed ? 'warn' : undefined}
            />
            {container.exit_code != null && (
              <Field
                label="Exit code"
                mono
                value={String(container.exit_code)}
              />
            )}
            {container.finished_at && (
              <Field
                label="Exited at"
                value={new Date(container.finished_at).toLocaleString()}
              />
            )}
            {container.error_message && (
              <Field
                label="Error"
                value={container.error_message}
                tone="warn"
              />
            )}
          </FieldGrid>
        </Section>
      )}

      {hasPorts && (
        <Section
          title="Ports"
          description="Port mappings between the host and the container."
        >
          <FieldGrid>
            {container.container_port ? (
              <Field
                label="Container port"
                mono
                value={String(container.container_port)}
              />
            ) : null}
            {container.host_port ? (
              <Field
                label="Host port"
                mono
                value={String(container.host_port)}
              />
            ) : null}
          </FieldGrid>
        </Section>
      )}

      {envVars.length > 0 && (
        <Section
          title="Environment variables"
          description={`${envVars.length} variable${envVars.length === 1 ? '' : 's'} injected at runtime. Sensitive values are masked — click the eye to reveal.`}
        >
          <div className="divide-y divide-neutral-950/5 overflow-hidden rounded-md border border-neutral-950/10 dark:divide-white/5 dark:border-white/10">
            {envVars.map(({ key }, i) => (
              <EnvVarRow
                key={`${key}-${i}`}
                envKey={key}
                projectId={projectId}
                environmentId={environmentId}
                containerId={containerId}
              />
            ))}
          </div>
        </Section>
      )}
    </div>
  )
}

function EnvVarRow({
  envKey,
  projectId,
  environmentId,
  containerId,
}: {
  envKey: string
  projectId: number
  environmentId: number
  containerId: string
}) {
  const [revealedValue, setRevealedValue] = useState<
    ScopedCredentialValue | undefined
  >()
  const [isRevealing, setIsRevealing] = useState(false)
  const revealGuard = useRef(createCredentialRevealGuard())
  const revealScope = `${projectId}:${environmentId}:${containerId}:${envKey}`
  const value = credentialValueForScope(revealedValue, revealScope)

  useEffect(() => {
    const guard = createCredentialRevealGuard()
    revealGuard.current = guard
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setRevealedValue(undefined)
    return () => guard.invalidate()
  }, [revealScope])

  const toggleReveal = async () => {
    if (value !== undefined) {
      revealGuard.current.cancel('value')
      setRevealedValue(undefined)
      return
    }

    const request = revealGuard.current.begin('value')
    setIsRevealing(true)
    try {
      const response = await getContainerEnvironmentVariable({
        path: {
          project_id: projectId,
          environment_id: environmentId,
          container_id: containerId,
          variable_name: envKey,
        },
        credentials: 'include',
        cache: 'no-store',
        throwOnError: true,
      })
      if (!revealGuard.current.isCurrent('value', request)) return
      setRevealedValue({ value: response.data.value, scope: revealScope })
    } catch {
      if (revealGuard.current.isCurrent('value', request)) {
        toast.error(`Failed to reveal ${envKey}`)
      }
    } finally {
      if (revealGuard.current.finish('value', request)) {
        setIsRevealing(false)
      }
    }
  }

  return (
    <div className="grid grid-cols-1 gap-1 px-3 py-2.5 sm:grid-cols-[minmax(10rem,16rem)_1fr] sm:gap-4 sm:items-start">
      <div className="font-mono text-[0.8125rem] font-medium text-neutral-900 break-all dark:text-white">
        {envKey}
      </div>
      <div className="group flex items-start gap-1 min-w-0">
        <div className="font-mono text-[0.8125rem] text-neutral-600 break-all dark:text-neutral-400 min-w-0 flex-1">
          {isRevealing ? 'Revealing…' : (value ?? '••••••••••••')}
        </div>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="h-6 w-6 shrink-0 opacity-60 transition hover:opacity-100 focus:opacity-100 group-hover:opacity-100"
          onClick={() => void toggleReveal()}
          disabled={isRevealing}
          aria-label={
            value !== undefined ? `Hide ${envKey}` : `Reveal ${envKey}`
          }
          title={value !== undefined ? 'Hide value' : 'Reveal value'}
        >
          {isRevealing ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : value !== undefined ? (
            <EyeOff className="h-3.5 w-3.5" />
          ) : (
            <Eye className="h-3.5 w-3.5" />
          )}
        </Button>
        {value !== undefined && (
          <CopyButton
            value={value}
            className="shrink-0 opacity-0 transition group-hover:opacity-100 focus:opacity-100"
          />
        )}
      </div>
    </div>
  )
}

function Section({
  title,
  description,
  children,
}: {
  title: string
  description?: string
  children: React.ReactNode
}) {
  return (
    <section className="grid grid-cols-1 gap-6 lg:grid-cols-[minmax(0,18rem)_1fr]">
      <header>
        <h3 className="text-base font-semibold text-neutral-900 dark:text-white">
          {title}
        </h3>
        {description && (
          <p className="mt-1 text-sm text-neutral-600 dark:text-neutral-400">
            {description}
          </p>
        )}
      </header>
      <div className="min-w-0">{children}</div>
    </section>
  )
}

function FieldGrid({ children }: { children: React.ReactNode }) {
  return (
    <dl className="grid grid-cols-1 gap-x-6 gap-y-5 sm:grid-cols-2">
      {children}
    </dl>
  )
}

function Field({
  label,
  value,
  mono,
  copyable,
  tone,
}: {
  label: string
  value: string
  mono?: boolean
  copyable?: boolean
  tone?: 'warn'
}) {
  return (
    <div className="flex flex-col gap-1 min-w-0">
      <dt className="text-xs font-medium uppercase tracking-wide text-neutral-500 dark:text-neutral-400">
        {label}
      </dt>
      <dd
        className={`flex items-center gap-2 min-w-0 ${
          tone === 'warn'
            ? 'text-amber-700 dark:text-amber-400'
            : 'text-neutral-900 dark:text-white'
        }`}
      >
        <span
          className={`min-w-0 flex-1 truncate ${mono ? 'font-mono text-[0.8125rem]' : 'text-sm'}`}
          title={value}
        >
          {value}
        </span>
        {copyable && <CopyButton value={value} minimal className="shrink-0" />}
      </dd>
    </div>
  )
}

function normalizeEnvVars(
  vars: ContainerDetailResponse['environment_variables']
): Array<{ key: string; value: string }> {
  if (!vars) return []
  const out: Array<{ key: string; value: string }> = []
  if (Array.isArray(vars)) {
    for (const ev of vars as unknown[]) {
      if (typeof ev === 'string') {
        const [k, ...rest] = (ev as string).split('=')
        if (k) out.push({ key: k, value: rest.join('=') })
        continue
      }
      if (ev && typeof ev === 'object') {
        const obj = ev as Record<string, unknown>
        if ('name' in obj && 'value' in obj) {
          out.push({ key: String(obj.name), value: String(obj.value ?? '') })
        } else if ('key' in obj && 'value' in obj) {
          out.push({ key: String(obj.key), value: String(obj.value ?? '') })
        } else {
          const entries = Object.entries(obj)
          if (entries.length >= 2) {
            out.push({
              key: String(entries[0][1]),
              value: String(entries[1][1] ?? ''),
            })
          } else if (entries.length === 1) {
            out.push({
              key: String(entries[0][0]),
              value: String(entries[0][1] ?? ''),
            })
          }
        }
      }
    }
  } else if (typeof vars === 'object') {
    for (const [k, v] of Object.entries(vars as Record<string, unknown>)) {
      out.push({ key: k, value: String(v ?? '') })
    }
  }
  return out
}

function formatUptimeFromTimestamp(createdAt?: string | null): string {
  if (!createdAt) return 'N/A'
  const elapsedMs = Date.now() - new Date(createdAt).getTime()
  if (!Number.isFinite(elapsedMs) || elapsedMs < 0) return 'N/A'
  const elapsedSeconds = Math.floor(elapsedMs / 1000)
  return formatUptime(elapsedSeconds)
}

function formatUptime(seconds: number): string {
  if (seconds < 60) return `${Math.floor(seconds)}s`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`
  return `${Math.floor(seconds / 86400)}d`
}

function formatCores(cores: number): string {
  if (cores >= 1) {
    const rounded = Math.round(cores * 100) / 100
    return `${rounded} ${rounded === 1 ? 'core' : 'cores'}`
  }
  return `${Math.round(cores * 1000)}m`
}

function formatMemoryMb(mb: number): string {
  if (mb >= 1024) {
    const gb = mb / 1024
    return `${gb % 1 === 0 ? gb.toFixed(0) : gb.toFixed(2)} GB`
  }
  return `${Math.round(mb)} MB`
}
