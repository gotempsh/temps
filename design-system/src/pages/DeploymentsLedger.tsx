// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useNavigate } from 'react-router'
import { PackageOpen } from 'lucide-react'
import { Ledger, PageState, Status, fmtDuration, fmtRelativeTime, useUrlState } from '@temps-sdk/ds'
import { Input } from '@temps-sdk/ui'
import { DEPLOYMENTS, type DeploymentFixture } from '../fixtures'

/** Reference screen for the `Ledger` template — a "Deployments" list. */
export default function DeploymentsLedger() {
  const navigate = useNavigate()
  const { state, patch } = useUrlState<'q' | 'page'>()
  const q = state.q?.toLowerCase() ?? ''

  const rows = DEPLOYMENTS.filter(
    (d) => !q || d.service.includes(q) || d.branch.includes(q) || d.author.includes(q),
  )

  return (
    <Ledger<DeploymentFixture>
      title="Deployments"
      description="Every deploy across every service in this project."
      toolbar={
        <Input
          placeholder="Filter by service, branch, or author…"
          value={state.q ?? ''}
          onChange={(e) => patch({ q: e.target.value || undefined })}
          className="max-w-sm"
        />
      }
      columns={[
        { key: 'service', header: 'Service', render: (d) => <span className="font-medium">{d.service}</span> },
        {
          key: 'branch',
          header: 'Branch',
          render: (d) => (
            <span className="font-mono text-xs text-muted-foreground">
              {d.branch} · {d.commit}
            </span>
          ),
        },
        { key: 'status', header: 'Status', render: (d) => <Status tone={d.status} label={d.statusLabel} /> },
        { key: 'duration', header: 'Duration', render: (d) => fmtDuration(d.durationMs) },
        { key: 'author', header: 'Author', render: (d) => d.author },
        {
          key: 'created',
          header: 'Deployed',
          render: (d) => <span title={d.createdAt}>{fmtRelativeTime(d.createdAt)}</span>,
        },
      ]}
      rows={rows}
      rowKey={(d) => d.id}
      onRowClick={() => navigate('/detail')}
      empty={
        <PageState
          variant="empty"
          icon={PackageOpen}
          title="No deployments match that filter"
          description="Clear the filter to see every deployment for this project."
        />
      }
    />
  )
}
