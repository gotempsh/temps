// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pick one of your sandboxes by its public id. Destroyed sandboxes are left
 * out: nothing can be granted to, or asked as, one of them.
 */
import { useQuery } from '@tanstack/react-query'
import { listSandboxesOptions } from '@/api/client/@tanstack/react-query.gen'
import type { SandboxInner } from '@/api/client/types.gen'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { namedSandbox } from '@/components/project/settings/internal-access'

/** The `Select` value for "no sandbox". */
const NONE = 'none'

/** The sandboxes a picker offers, live ones only, newest first as listed. */
export function pickableSandboxes(sandboxes: SandboxInner[]): SandboxInner[] {
  return sandboxes.filter((sandbox) => sandbox.status !== 'destroyed')
}

/** "agent scratchpad · sbx_…", with " · stopped" when it is not running. */
export function sandboxOptionLabel(sandbox: SandboxInner): string {
  const named = namedSandbox(sandbox.name, sandbox.id)
  return sandbox.status === 'running' ? named : `${named} · ${sandbox.status}`
}

export function SandboxSelect({
  value,
  onValueChange,
  ariaLabel,
  placeholder = 'Select sandbox…',
  noneLabel,
  disabled,
  className,
}: {
  /** A sandbox's public id, or `null` for none. */
  value: string | null
  onValueChange: (value: string | null) => void
  ariaLabel: string
  placeholder?: string
  /** Offer "none" with this label. */
  noneLabel?: string
  disabled?: boolean
  className?: string
}) {
  const sandboxes = useQuery(
    listSandboxesOptions({ query: { page: 1, page_size: 100 } })
  )
  const options = pickableSandboxes(sandboxes.data?.sandboxes ?? [])
  const empty = sandboxes.isSuccess && options.length === 0
  return (
    <Select
      value={value ?? (noneLabel ? NONE : '')}
      onValueChange={(next) => onValueChange(next === NONE ? null : next)}
      disabled={disabled || sandboxes.isPending}
    >
      <SelectTrigger aria-label={ariaLabel} className={className}>
        <SelectValue
          placeholder={
            sandboxes.isPending
              ? 'Loading sandboxes…'
              : sandboxes.isError
                ? 'Could not load your sandboxes'
                : empty
                  ? 'You have no sandboxes'
                  : placeholder
          }
        />
      </SelectTrigger>
      <SelectContent>
        {noneLabel ? <SelectItem value={NONE}>{noneLabel}</SelectItem> : null}
        {options.map((sandbox) => (
          <SelectItem key={sandbox.id} value={sandbox.id}>
            {sandboxOptionLabel(sandbox)}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}
