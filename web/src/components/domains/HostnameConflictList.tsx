// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DnsRecordConflict } from '@/api/client'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Callout } from '@temps-sdk/ds'
import {
  conflictKey,
  describeRecordValue,
  type ConflictDecision,
  type ConflictDecisions,
} from './hostname-conflicts'

interface HostnameConflictListProps {
  conflicts: readonly DnsRecordConflict[]
  decisions: ConflictDecisions
  onDecide: (conflict: DnsRecordConflict, decision: ConflictDecision) => void
  disabled?: boolean
}

/**
 * Generated hostnames whose DNS records Temps will not write without the
 * user's say. Each one is adopted (Temps takes the record over and points
 * it at the new value) or skipped (its record is left exactly as it is).
 * There is deliberately no "adopt all": every record is confirmed on its
 * own, next to its current value.
 */
export function HostnameConflictList({
  conflicts,
  decisions,
  onDecide,
  disabled,
}: HostnameConflictListProps) {
  const title =
    conflicts.length === 1
      ? '1 DNS record needs a decision'
      : `${conflicts.length} DNS records need a decision`
  return (
    <div className="space-y-3">
      <Callout tone="warning" title={title}>
        Temps never overwrites a DNS record it does not manage. Adopt a record
        to let Temps take it over and point it at the new value, or skip its
        hostname to leave the record exactly as it is. All other changes are
        applied either way.
      </Callout>
      <ul className="space-y-3">
        {conflicts.map((conflict) => (
          <HostnameConflictItem
            key={conflictKey(conflict)}
            conflict={conflict}
            decision={decisions[conflictKey(conflict)]}
            onDecide={onDecide}
            disabled={disabled}
          />
        ))}
      </ul>
    </div>
  )
}

function HostnameConflictItem({
  conflict,
  decision,
  onDecide,
  disabled,
}: {
  conflict: DnsRecordConflict
  decision: ConflictDecision | undefined
  onDecide: (conflict: DnsRecordConflict, decision: ConflictDecision) => void
  disabled?: boolean
}) {
  const newValue = describeRecordValue(conflict.value, conflict.proxied)
  return (
    <li className="rounded-lg border border-warning/40 bg-warning/5 p-3 text-sm">
      <p className="font-mono text-xs font-medium break-all">
        {conflict.record_type} {conflict.name}
      </p>
      <p className="mt-1 text-muted-foreground">{conflict.reason}</p>
      {conflict.current_value != null && (
        <p className="mt-2 font-mono text-xs break-all">
          {describeRecordValue(
            conflict.current_value,
            conflict.current_proxied
          )}{' '}
          → {newValue}
        </p>
      )}
      <RadioGroup
        className="mt-3"
        value={decision ?? ''}
        onValueChange={(value) => {
          if (value === 'adopt' || value === 'skip') onDecide(conflict, value)
        }}
        disabled={disabled}
        aria-label={`Decision for ${conflict.record_type} ${conflict.name}`}
      >
        {conflict.adoptable && (
          <label className="flex cursor-pointer items-start gap-2">
            <RadioGroupItem value="adopt" className="mt-0.5" />
            <span>
              Adopt this record and point it at{' '}
              <span className="font-mono text-xs">{newValue}</span>
            </span>
          </label>
        )}
        <label className="flex cursor-pointer items-start gap-2">
          <RadioGroupItem value="skip" className="mt-0.5" />
          <span>Skip this hostname and leave its record untouched</span>
        </label>
      </RadioGroup>
      {!conflict.adoptable && (
        <p className="mt-2 text-xs text-muted-foreground">
          This record can’t be adopted. Skip it, or fix it at your DNS provider
          and preview again.
        </p>
      )}
    </li>
  )
}
