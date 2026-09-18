// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { AlertCircle, Clock, Search } from 'lucide-react'
import {
  Button,
  Callout,
  DataTable,
  Field,
  PageContainer,
  PageHeader,
  PageState,
  Status,
  fmtDateTime,
  fmtDuration,
  useUrlState,
  type DataTableColumn,
} from '@temps-sdk/ds'
import {
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import { CRON_EXECUTIONS, type CronExecutionFixture } from '../fixtures'

const SCENARIOS = [
  {
    value: 'loaded',
    label: 'Runs available',
    note: 'Each run names the task, gives a text status, and includes the HTTP response or failure reason.',
  },
  {
    value: 'loading',
    label: 'First load',
    note: 'Keep the section heading and table columns visible while the first request is pending.',
  },
  {
    value: 'empty',
    label: 'No runs yet',
    note: 'Use this state only after a successful response contains no records. Explain when records will appear.',
  },
  {
    value: 'failed',
    label: 'Request failed',
    note: 'Explain what could not be loaded and offer a retry. A failed request is never an empty collection.',
  },
  {
    value: 'stale',
    label: 'Refresh failed',
    note: 'Retain usable rows and the current filter. Explain that the displayed data may be out of date.',
  },
] as const

const columns: DataTableColumn<CronExecutionFixture>[] = [
  { key: 'task', header: 'Task', render: (run) => <code>{run.path}</code> },
  {
    key: 'time',
    header: 'Started',
    render: (run) => fmtDateTime(run.executedAt),
  },
  {
    key: 'status',
    header: 'Result',
    render: (run) => (
      <Status
        tone={run.statusCode >= 200 && run.statusCode < 300 ? 'ok' : 'error'}
        label={
          run.statusCode >= 200 && run.statusCode < 300 ? 'Succeeded' : 'Failed'
        }
      />
    ),
  },
  {
    key: 'duration',
    header: 'Duration',
    render: (run) => fmtDuration(run.durationMs),
  },
  {
    key: 'details',
    header: 'Details',
    render: (run) => (
      <span className="whitespace-normal">
        HTTP {run.statusCode}
        {run.error ? ` · ${run.error}` : ''}
      </span>
    ),
  },
]

export default function TableStates() {
  const { get, patch } = useUrlState<'scenario' | 'query'>()
  const scenario =
    SCENARIOS.find((item) => item.value === get('scenario')) ?? SCENARIOS[0]
  const query = get('query') ?? ''
  const runs = CRON_EXECUTIONS.filter((run) =>
    run.path.toLowerCase().includes(query.trim().toLowerCase()),
  )
  const retry = () => patch({ scenario: 'loaded' })

  return (
    <PageContainer>
      <PageHeader
        title="Execution history"
        description="An embedded table with clear loading, empty, and recovery states."
      />
      <section
        aria-label="Example controls"
        className="space-y-3 rounded-lg border bg-muted/20 p-4"
      >
        <Field
          label="Preview a request state"
          description="Invented sample data. These controls change this example only; the URL preserves your selection."
        >
          {(props) => (
            <Select
              value={scenario.value}
              onValueChange={(value) => patch({ scenario: value })}
            >
              <SelectTrigger {...props} className="w-full sm:w-64">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {SCENARIOS.map((item) => (
                  <SelectItem key={item.value} value={item.value}>
                    {item.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
        </Field>
        <p className="text-sm text-muted-foreground">{scenario.note}</p>
        {scenario.value === 'loading' && (
          <Button variant="outline" onClick={retry}>
            Finish sample request
          </Button>
        )}
      </section>

      <section
        aria-labelledby="execution-history-title"
        className="min-w-0 space-y-4"
      >
        <div>
          <h2 id="execution-history-title" className="text-lg font-semibold">
            Recent executions
          </h2>
          <p className="text-sm text-muted-foreground">
            Recent scheduled tasks for this project. Times use your browser’s
            time zone.
          </p>
        </div>
        <Field label="Filter by task path">
          {(props) => (
            <Input
              {...props}
              value={query}
              onChange={(event) => patch({ query: event.target.value || null })}
              placeholder="e.g. /tasks/cleanup"
              className="w-full sm:max-w-sm"
            />
          )}
        </Field>
        {scenario.value === 'stale' && (
          <Callout tone="error" title="Couldn't refresh execution history">
            <div className="space-y-3">
              <p>
                Showing the last loaded runs. New runs may be missing until the
                connection recovers.
              </p>
              <Button variant="outline" size="sm" onClick={retry}>
                Retry refresh
              </Button>
            </div>
          </Callout>
        )}
        {scenario.value === 'failed' ? (
          <div className="rounded-md border">
            <PageState
              variant="failed"
              size="compact"
              icon={AlertCircle}
              title="Couldn't load execution history"
              description="The request timed out. Retry to see recent runs."
              action={<Button onClick={retry}>Retry loading history</Button>}
            />
          </div>
        ) : scenario.value === 'empty' ? (
          <div className="rounded-md border">
            <PageState
              variant="empty"
              size="compact"
              icon={Clock}
              title="No executions yet"
              description="Runs will appear after the first scheduled task executes."
            />
          </div>
        ) : scenario.value !== 'loading' && runs.length === 0 ? (
          <div className="rounded-md border">
            <PageState
              variant="empty"
              size="compact"
              icon={Search}
              title="No matching executions"
              description={`No loaded task paths match “${query}”. Clear the filter to see all loaded runs.`}
              action={
                <Button
                  variant="outline"
                  onClick={() => patch({ query: null })}
                >
                  Clear filter
                </Button>
              }
            />
          </div>
        ) : (
          <DataTable
            aria-label="Recent executions"
            columns={columns}
            rows={runs}
            rowKey={(run) => run.id}
            isLoading={scenario.value === 'loading'}
          />
        )}
      </section>
    </PageContainer>
  )
}
