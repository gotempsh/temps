// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  listDataImportsQueryKey,
  listRootContainersOptions,
  startDataImportMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  DataImportRunResponse,
  DataImportSpec,
} from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Checkbox } from '@/components/ui/checkbox'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, Eye, EyeOff, Loader2, Upload } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import {
  activeImportConflict,
  buildStartRequest,
  importFormSchema,
  problemCode,
  problemDetail,
  schemeProblem,
  sourcePathHint,
  suggestedTargetDatabases,
  targetDatabaseHint,
  type ImportFormValues,
} from './import-state'

interface DataImportFormProps {
  serviceId: number
  serviceType: string
  spec: DataImportSpec
  /** Whether an import can start now; the form stays visible either way. */
  available: boolean
  defaultTimeoutMinutes: number
  maxTimeoutMinutes: number
  /** Called with the run the server accepted. */
  onStarted: (run: DataImportRunResponse) => void
  /** Pre-filled target, e.g. from "Import again" on a run. */
  initialTarget?: string
}

export function DataImportForm({
  serviceId,
  serviceType,
  spec,
  available,
  defaultTimeoutMinutes,
  maxTimeoutMinutes,
  onStarted,
  initialTarget,
}: DataImportFormProps) {
  const queryClient = useQueryClient()
  const [showSource, setShowSource] = useState(false)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const schema = useMemo(
    () => importFormSchema(maxTimeoutMinutes, spec.max_target_length),
    [maxTimeoutMinutes, spec.max_target_length]
  )
  const form = useForm<ImportFormValues>({
    resolver: zodResolver(schema),
    defaultValues: {
      sourceUrl: '',
      targetDatabase: initialTarget ?? '',
      replace: false,
      confirmTargetDatabase: '',
      timeoutMinutes: defaultTimeoutMinutes,
    },
  })

  // Existing databases, offered as targets. Best effort: the explorer may not
  // support this engine, and typing a name always works.
  const { data: containers } = useQuery({
    ...listRootContainersOptions({ path: { service_id: serviceId } }),
    enabled: available,
    retry: false,
  })
  const suggestions = suggestedTargetDatabases(
    serviceType,
    containers?.map((container) => container.name)
  )

  const sourceUrl = useWatch({ control: form.control, name: 'sourceUrl' })
  const targetDatabase = useWatch({
    control: form.control,
    name: 'targetDatabase',
  })
  const replace = useWatch({ control: form.control, name: 'replace' })
  const sourceSchemeProblem = schemeProblem(sourceUrl, spec.source_schemes)

  const start = useMutation({
    ...startDataImportMutation(),
    retry: false,
    onSuccess: (run) => {
      queryClient.invalidateQueries({
        queryKey: listDataImportsQueryKey({ path: { id: serviceId } }),
      })
      form.reset({
        ...form.getValues(),
        sourceUrl: '',
        replace: false,
        confirmTargetDatabase: '',
      })
      setShowSource(false)
      toast.success('Import started', {
        description: `Copying into '${run.target_database}'.`,
      })
      onStarted(run)
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => start.mutate(variables))) {
        return
      }
      if (activeImportConflict(error) !== undefined) {
        queryClient.invalidateQueries({
          queryKey: listDataImportsQueryKey({ path: { id: serviceId } }),
        })
        toast.info('An import into this database is already running', {
          description: 'Its progress is shown below.',
        })
        return
      }
      if (problemCode(error) === 'target-not-empty') {
        form.setError('replace', {
          message:
            'This database already holds data. Turn on replace to drop it and import into a fresh one.',
        })
      }
      toast.error('Could not start the import', {
        description: problemDetail(error),
      })
    },
  })

  const onSubmit = (values: ImportFormValues) => {
    start.mutate({
      path: { id: serviceId },
      body: buildStartRequest(values),
    })
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Copy a database into this service</CardTitle>
        <CardDescription>
          Temps connects to the source with the connection string below, dumps
          the database and loads it into a database of this service. The
          connection string is used for this import only and is never stored.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <Form {...form}>
          <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
            <FormField
              control={form.control}
              name="sourceUrl"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Source connection string</FormLabel>
                  <div className="flex gap-2">
                    <FormControl>
                      <Input
                        type={showSource ? 'text' : 'password'}
                        autoComplete="off"
                        spellCheck={false}
                        placeholder={spec.source_url_example}
                        className="font-mono"
                        {...field}
                      />
                    </FormControl>
                    <Button
                      type="button"
                      variant="outline"
                      size="icon"
                      aria-label={
                        showSource
                          ? 'Hide connection string'
                          : 'Show connection string'
                      }
                      onClick={() => setShowSource((shown) => !shown)}
                    >
                      {showSource ? (
                        <EyeOff className="h-4 w-4" />
                      ) : (
                        <Eye className="h-4 w-4" />
                      )}
                    </Button>
                  </div>
                  <FormDescription>
                    A {spec.engine_label} server reachable from the internet,
                    for example{' '}
                    <code className="break-all text-xs">
                      {spec.source_url_example}
                    </code>
                    . {sourcePathHint(serviceType)}
                    {spec.allowed_source_options.length > 0 && (
                      <>
                        {' '}
                        Accepted options:{' '}
                        {spec.allowed_source_options.join(', ')}.
                      </>
                    )}
                  </FormDescription>
                  {sourceSchemeProblem && (
                    <p className="text-sm text-destructive">
                      {sourceSchemeProblem}
                    </p>
                  )}
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="targetDatabase"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Target database</FormLabel>
                  <FormControl>
                    <Input
                      autoComplete="off"
                      spellCheck={false}
                      placeholder="myapp_production"
                      className="font-mono"
                      {...field}
                    />
                  </FormControl>
                  <FormDescription>
                    {targetDatabaseHint(serviceType)}
                  </FormDescription>
                  {suggestions.length > 0 && (
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-xs text-muted-foreground">
                        Existing:
                      </span>
                      {suggestions.map((name) => (
                        <Badge
                          key={name}
                          variant={
                            name === targetDatabase ? 'default' : 'outline'
                          }
                          className="cursor-pointer font-mono"
                          onClick={() =>
                            form.setValue('targetDatabase', name, {
                              shouldValidate: true,
                            })
                          }
                        >
                          {name}
                        </Badge>
                      ))}
                    </div>
                  )}
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="replace"
              render={({ field }) => (
                <FormItem className="flex flex-row items-start gap-3 rounded-md border p-4">
                  <FormControl>
                    <Checkbox
                      checked={field.value}
                      onCheckedChange={(checked) => {
                        field.onChange(checked === true)
                        form.clearErrors('replace')
                      }}
                    />
                  </FormControl>
                  <div className="space-y-1 leading-none">
                    <FormLabel>Replace the database if it holds data</FormLabel>
                    <FormDescription>
                      Without this, an import into a database that already has{' '}
                      {spec.object_noun}s is refused. With it, the database is
                      dropped and created again first — its current contents are
                      lost.
                    </FormDescription>
                    <FormMessage />
                  </div>
                </FormItem>
              )}
            />

            {replace && (
              <FormField
                control={form.control}
                name="confirmTargetDatabase"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>
                      Type{' '}
                      <code className="text-xs">
                        {targetDatabase.trim() || 'the database name'}
                      </code>{' '}
                      to confirm
                    </FormLabel>
                    <FormControl>
                      <Input
                        autoComplete="off"
                        spellCheck={false}
                        className="font-mono"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            )}

            <FormField
              control={form.control}
              name="timeoutMinutes"
              render={({ field }) => (
                <FormItem className="max-w-xs">
                  <FormLabel>Time limit (minutes)</FormLabel>
                  <FormControl>
                    <Input
                      type="number"
                      min={1}
                      max={maxTimeoutMinutes}
                      value={Number.isNaN(field.value) ? '' : field.value}
                      onChange={(event) =>
                        field.onChange(event.target.valueAsNumber)
                      }
                    />
                  </FormControl>
                  <FormDescription>
                    The copy is stopped after this long.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            <Alert variant={spec.atomic ? 'default' : 'warning'}>
              <AlertTriangle className="h-4 w-4" />
              <AlertTitle>
                {spec.atomic
                  ? 'All or nothing'
                  : 'A failed import can leave part of the data'}
              </AlertTitle>
              <AlertDescription>
                {spec.atomic
                  ? `${spec.engine_label} imports run in a single transaction: if anything fails, nothing is kept.`
                  : `${spec.engine_label} cannot import in a single transaction. If an import fails part-way, run it again with replace turned on.`}
              </AlertDescription>
            </Alert>

            <div className="flex justify-end">
              <Button type="submit" disabled={!available || start.isPending}>
                {start.isPending ? (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                ) : (
                  <Upload className="mr-2 h-4 w-4" />
                )}
                Start import
              </Button>
            </div>
          </form>
        </Form>
      </CardContent>
      {verificationDialog}
    </Card>
  )
}
