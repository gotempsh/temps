// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { Button, Callout, Status } from '@temps-sdk/ds'
import { Search } from 'lucide-react'
import { useForm } from 'react-hook-form'
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
import { problemDetail } from '@/lib/api-problem'
import {
  DNS_TEST_RECORD_TYPES,
  outcomeLabel,
  outcomeTone,
  testDnsResolution,
  testResolutionSchema,
  type DnsResolveResponse,
  type DnsResolverEntry,
  type TestResolutionValues,
} from '@/lib/dns-observability'

function ResolveResult({ result }: { result: DnsResolveResponse }) {
  return (
    <div className="space-y-3 rounded-md border p-3" aria-live="polite">
      <div className="flex flex-wrap items-center gap-2">
        <Status
          tone={outcomeTone(result.outcome)}
          label={outcomeLabel(result.outcome)}
        />
        <span className="text-sm text-muted-foreground tabular-nums">
          {result.latency_ms.toFixed(1)} ms
          {result.authoritative ? ' · authoritative' : ''}
          {result.asked_as ? ` · asked as ${result.asked_as}` : ''}
        </span>
      </div>
      {result.answers.length > 0 ? (
        <ul className="space-y-1 font-mono text-xs">
          {result.answers.map((answer, index) => (
            <li
              key={`${answer.name}-${answer.record_type}-${index}`}
              className="[overflow-wrap:anywhere]"
            >
              {answer.name} {answer.ttl} {answer.record_type}{' '}
              <span className="font-semibold">{answer.data}</span>
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-sm text-muted-foreground">
          No {result.record_type} records for {result.name}
        </p>
      )}
      {result.note ? <Callout tone="info">{result.note}</Callout> : null}
    </div>
  )
}

/** Ask one node's resolver a name, optionally as a project's container would. */
export function DnsTestResolution({
  resolvers,
}: {
  resolvers: DnsResolverEntry[]
}) {
  const form = useForm<TestResolutionValues>({
    resolver: zodResolver(testResolutionSchema),
    defaultValues: {
      node_id: resolvers[0]?.node_id ?? 0,
      name: '',
      record_type: 'A',
      project_id: null,
    },
  })
  const resolve = useMutation({
    mutationFn: (values: TestResolutionValues) =>
      testDnsResolution({
        node_id: values.node_id,
        name: values.name,
        record_type: values.record_type,
        project_id: values.project_id ?? undefined,
      }),
  })

  return (
    <div className="space-y-4">
      <Form {...form}>
        <form
          onSubmit={form.handleSubmit((values) => resolve.mutate(values))}
          className="grid grid-cols-1 gap-3 md:grid-cols-2 lg:grid-cols-[2fr_1fr_1fr_1fr_auto] lg:items-end"
        >
          <FormField
            control={form.control}
            name="name"
            render={({ field }) => (
              <FormItem>
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
              <FormItem>
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
              <FormItem>
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
                        {resolver.node_name}
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
              <FormItem>
                <FormLabel>As project</FormLabel>
                <ProjectSelect
                  ariaLabel="As project"
                  value={field.value}
                  onValueChange={field.onChange}
                  allowAll
                  allLabel="Any (node itself)"
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
      {resolve.data ? <ResolveResult result={resolve.data} /> : null}
    </div>
  )
}
