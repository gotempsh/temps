// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { Button, Callout, Status, useUrlState } from '@temps-sdk/ds'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@temps-sdk/ui'
import { Search } from 'lucide-react'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { Link } from 'react-router'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { ProjectSelect } from '@/components/project/ProjectSelect'
import {
  internalAccessDiagnostic,
  sandboxAccessDiagnostic,
} from '@/components/project/settings/internal-access'
import { SandboxSelect } from '@/components/sandboxes/SandboxSelect'
import { problemDetail } from '@/lib/api-problem'
import {
  DNS_PARAMS,
  DNS_TEST_RECORD_TYPES,
  nodeDnsHealth,
  outcomeLabel,
  outcomeMeaning,
  outcomeTone,
  testDnsResolution,
  testResolutionSchema,
  testValuesFromUrl,
  type DnsParam,
  type DnsResolveResponse,
  type DnsResolverEntry,
  type TestResolutionValues,
} from '@/lib/dns-observability'

function ResolveResult({
  result,
  sandboxId,
}: {
  result: DnsResolveResponse
  sandboxId: string | null
}) {
  return (
    <section
      aria-label="Result"
      aria-live="polite"
      className="space-y-3 rounded-md border p-4"
    >
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <Status
          tone={outcomeTone(result.outcome)}
          label={outcomeMeaning(result.outcome, result.record_type)}
        />
        <span className="font-mono text-xs text-muted-foreground">
          {result.rcode || outcomeLabel(result.outcome)}
        </span>
        <span className="text-sm text-muted-foreground tabular-nums">
          {result.latency_ms.toFixed(1)} ms
          {result.authoritative ? ' · answered from the internal zone' : ''}
          {result.asked_as ? ` · asked as ${result.asked_as}` : ''}
        </span>
      </div>
      {result.answers.length > 0 ? (
        <div className="@container rounded-md border">
          <Table aria-label="Answers">
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead className="w-0">Type</TableHead>
                <TableHead>Answer</TableHead>
                <TableHead className="hidden w-0 @md:table-cell">TTL</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {result.answers.map((answer, index) => (
                <TableRow key={`${answer.name}-${answer.record_type}-${index}`}>
                  <TableCell className="font-mono text-xs [overflow-wrap:anywhere]">
                    {answer.name}
                  </TableCell>
                  <TableCell className="font-mono text-xs text-muted-foreground">
                    {answer.record_type}
                  </TableCell>
                  <TableCell className="font-mono text-xs font-semibold [overflow-wrap:anywhere]">
                    {answer.data}
                  </TableCell>
                  <TableCell className="hidden font-mono text-xs text-muted-foreground tabular-nums @md:table-cell">
                    {answer.ttl}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      ) : (
        <p className="text-sm text-muted-foreground [overflow-wrap:anywhere]">
          No {result.record_type} records for {result.name}
        </p>
      )}
      {result.note ? <Callout tone="info">{result.note}</Callout> : null}
      <InternalAccessResult result={result} sandboxId={sandboxId} />
    </section>
  )
}

/**
 * A name that resolves is half a working call: another project's internal
 * address answers 403 unless that project allows the caller (ADR 101), and
 * a sandbox's call unless the project allows that sandbox (ADR 105).
 */
function InternalAccessResult({
  result,
  sandboxId,
}: {
  result: DnsResolveResponse
  sandboxId: string | null
}) {
  const access = result.internal_access
  const diagnostic = !access
    ? null
    : sandboxId
      ? sandboxAccessDiagnostic(access, sandboxId)
      : internalAccessDiagnostic(access, 'the selected project')
  if (!diagnostic) return null
  return (
    <Callout tone={diagnostic.tone} title={diagnostic.title}>
      <p>{diagnostic.detail}</p>
      {diagnostic.setupPath ? (
        <Link
          to={diagnostic.setupPath}
          className="font-medium text-foreground underline underline-offset-4"
        >
          Open {result.internal_access?.target_project_name}&apos;s internal
          access
        </Link>
      ) : null}
    </Callout>
  )
}

/** The option label for a node, saying when it cannot answer. */
function nodeOptionLabel(resolver: DnsResolverEntry): string {
  const health = nodeDnsHealth(resolver, true)
  if (health.note)
    return `${resolver.node_name} (${health.label.toLowerCase()})`
  if (health.lookups.tone === 'error')
    return `${resolver.node_name} (lookups down)`
  return resolver.node_name
}

/**
 * Every field is a flex column: a select's hidden native `<select>` would
 * take a `space-y` margin, and a block label sits on a taller line box,
 * either of which puts one label out of line with its neighbours.
 */
const FIELD_ITEM = 'flex flex-col gap-2 space-y-0'

/**
 * Ask one node's resolver a name, optionally as a project's container
 * would. The fields come from the URL, so a record's Test link opens this
 * prefilled, and a lookup that ran is a link that runs it again.
 */
export function DnsTestResolution({
  resolvers,
}: {
  resolvers: DnsResolverEntry[]
}) {
  const { get, patch } = useUrlState<DnsParam>()
  const fromUrl = testValuesFromUrl(get, resolvers)
  const urlKey = JSON.stringify(fromUrl)
  const form = useForm<TestResolutionValues>({
    resolver: zodResolver(testResolutionSchema),
    defaultValues: fromUrl,
  })
  // Back/forward, or another tab's Test link, changes the URL under us.
  useEffect(() => {
    form.reset(JSON.parse(urlKey) as TestResolutionValues)
  }, [urlKey, form])
  const resolve = useMutation({
    mutationFn: (values: TestResolutionValues) =>
      testDnsResolution({
        node_id: values.node_id,
        name: values.name,
        record_type: values.record_type,
        project_id: values.project_id ?? undefined,
        sandbox_id: values.sandbox_id ?? undefined,
      }),
  })
  const onSubmit = (values: TestResolutionValues) => {
    patch({
      [DNS_PARAMS.testName]: values.name.trim(),
      [DNS_PARAMS.testType]:
        values.record_type === 'A' ? undefined : values.record_type,
      [DNS_PARAMS.testNode]: values.node_id,
      [DNS_PARAMS.testProject]: values.project_id ?? undefined,
      [DNS_PARAMS.testSandbox]: values.sandbox_id ?? undefined,
    })
    resolve.mutate(values)
  }

  return (
    <div className="space-y-4">
      <Form {...form}>
        <form
          onSubmit={form.handleSubmit(onSubmit)}
          className="grid grid-cols-1 gap-3 md:grid-cols-2 lg:grid-cols-[2fr_1fr_1fr_1fr_1fr_auto] lg:items-end"
        >
          <FormField
            control={form.control}
            name="name"
            render={({ field }) => (
              <FormItem className={FIELD_ITEM}>
                <FormLabel>Name</FormLabel>
                <FormControl>
                  <Input
                    placeholder="production.shop.temps.local"
                    autoComplete="off"
                    spellCheck={false}
                    {...field}
                  />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name="record_type"
            render={({ field }) => (
              <FormItem className={FIELD_ITEM}>
                <FormLabel>Type</FormLabel>
                <Select value={field.value} onValueChange={field.onChange}>
                  <FormControl>
                    <SelectTrigger className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                  </FormControl>
                  <SelectContent>
                    {DNS_TEST_RECORD_TYPES.map((type) => (
                      <SelectItem key={type} value={type}>
                        {type}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name="node_id"
            render={({ field }) => (
              <FormItem className={FIELD_ITEM}>
                <FormLabel>From node</FormLabel>
                <Select
                  value={String(field.value)}
                  onValueChange={(value) => field.onChange(Number(value))}
                >
                  <FormControl>
                    <SelectTrigger className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                  </FormControl>
                  <SelectContent>
                    {resolvers.map((resolver) => (
                      <SelectItem
                        key={resolver.node_id}
                        value={String(resolver.node_id)}
                      >
                        {nodeOptionLabel(resolver)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name="project_id"
            render={({ field }) => (
              <FormItem className={FIELD_ITEM}>
                <FormLabel>As project</FormLabel>
                <ProjectSelect
                  ariaLabel="As project"
                  value={field.value}
                  onValueChange={(next) => {
                    field.onChange(next)
                    // A test is asked as a project or a sandbox, not both.
                    if (next !== null) form.setValue('sandbox_id', null)
                  }}
                  allowAll
                  allLabel="Any (node itself)"
                  className="w-full"
                />
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name="sandbox_id"
            render={({ field }) => (
              <FormItem className={FIELD_ITEM}>
                <FormLabel>As sandbox</FormLabel>
                <SandboxSelect
                  ariaLabel="As sandbox"
                  value={field.value}
                  onValueChange={(next) => {
                    field.onChange(next)
                    if (next !== null) form.setValue('project_id', null)
                  }}
                  noneLabel="None"
                  className="w-full"
                />
                <FormMessage />
              </FormItem>
            )}
          />
          <Button type="submit" busy={resolve.isPending} busyLabel="Resolving…">
            <Search className="size-4" aria-hidden />
            Resolve
          </Button>
        </form>
      </Form>
      {resolve.isError ? (
        <Callout tone="error" title="Could not resolve the name">
          {problemDetail(resolve.error, 'The resolver did not answer.')}
        </Callout>
      ) : null}
      {resolve.data ? (
        <ResolveResult
          result={resolve.data}
          sandboxId={resolve.variables?.sandbox_id ?? null}
        />
      ) : null}
    </div>
  )
}
