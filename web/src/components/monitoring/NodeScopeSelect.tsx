// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Node selector for the Proxy and Server pages (ADR 061): "All nodes" plus
 * each node by name, held in `?node=` (see `useMetricNodeScope`). Hidden
 * when only the control plane exists.
 */

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  ALL_NODES,
  reportingNotice,
  scopeValue,
  type NodeOption,
  type NodeScope,
} from '@/lib/node-scope'
import { AlertTriangle } from 'lucide-react'

export function NodeScopeSelect({
  options,
  scope,
  onChange,
}: {
  options: NodeOption[]
  scope: NodeScope
  onChange: (value: string) => void
}) {
  if (options.length <= 1) return null
  return (
    <Select value={scopeValue(scope)} onValueChange={onChange}>
      <SelectTrigger className="w-full sm:w-[180px]" aria-label="Node">
        <SelectValue placeholder="All nodes" />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={ALL_NODES}>All nodes</SelectItem>
        {options.map((o) => (
          <SelectItem key={o.id} value={String(o.id)}>
            {o.name}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

/** Says which workers in the scope are not reporting, and why. */
export function NodeReportingNotice({
  scope,
  options,
  statuses,
}: {
  scope: NodeScope
  options: NodeOption[]
  statuses: Parameters<typeof reportingNotice>[2]
}) {
  const notice = reportingNotice(scope, options, statuses)
  if (!notice) return null
  return (
    <Alert>
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>{notice.title}</AlertTitle>
      <AlertDescription>{notice.detail}</AlertDescription>
    </Alert>
  )
}
