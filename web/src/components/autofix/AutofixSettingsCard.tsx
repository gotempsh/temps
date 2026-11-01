// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getAutofixCapabilityOptions,
  getAutofixCapabilityQueryKey,
  setAutofixPausedMutation,
  updateAutofixSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { AutofixSettingsResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { Check, Minus } from 'lucide-react'
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
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { getErrorMessage } from '@/utils/errorHandling'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import {
  CONFIDENCE_LEVELS,
  deliveryLabel,
  MAX_CONFIDENCE,
} from './autofix-incident'

const schema = z.object({
  mode: z.enum(['off', 'propose', 'auto_merge']),
  minConfidence: z
    .number({ error: `Choose a level from 0 to ${MAX_CONFIDENCE}` })
    .int()
    .min(0)
    .max(MAX_CONFIDENCE),
  protectedPaths: z.string().max(10_000),
  testCommand: z.string().max(1000),
})

type FormValues = z.infer<typeof schema>

const MODES: { value: FormValues['mode']; label: string; detail: string }[] = [
  {
    value: 'off',
    label: 'Only when asked',
    detail: 'Autofix runs when someone presses "Fix this crash".',
  },
  {
    value: 'propose',
    label: 'Every new crash',
    detail:
      'New and regressed crashes in production start an incident on their own; you review every fix.',
  },
  {
    value: 'auto_merge',
    label: 'Every new crash, merge confident fixes',
    detail:
      'Like the above. Fixes at or above the threshold that touch no protected path are marked for merging; until Temps can merge pull requests you still merge them.',
  },
]

function toForm(settings: AutofixSettingsResponse): FormValues {
  const mode = settings.mode as FormValues['mode']
  return {
    mode: MODES.some((m) => m.value === mode) ? mode : 'off',
    minConfidence: settings.min_confidence,
    protectedPaths: settings.protected_paths.join('\n'),
    testCommand: settings.test_command ?? '',
  }
}

/**
 * Project policy for crash autofix (ADR-054), the content of the Automation
 * page's "Crash autofix" section. Always rendered: it onboards when AI or
 * sandboxes are not set up.
 */
export function AutofixSettingsCard({
  projectId,
  showPause = true,
}: {
  projectId: number
  /** Off where the page already has an on/off switch for crash autofix. */
  showPause?: boolean
}) {
  const queryClient = useQueryClient()
  const capability = useQuery(
    getAutofixCapabilityOptions({ path: { project_id: projectId } })
  )
  const invalidate = () =>
    queryClient.invalidateQueries({
      queryKey: getAutofixCapabilityQueryKey({
        path: { project_id: projectId },
      }),
    })
  const form = useForm<FormValues>({
    resolver: zodResolver(schema),
    defaultValues: {
      mode: 'off',
      minConfidence: 4,
      protectedPaths: '',
      testCommand: '',
    },
  })
  const update = useMutation({
    ...updateAutofixSettingsMutation(),
    onSuccess: (saved) => {
      toast.success('Autofix settings saved')
      // What was saved is the form's new baseline: nothing is unsaved now.
      form.reset(toForm(saved))
      invalidate()
    },
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not save autofix settings')),
  })
  const pause = useMutation({
    ...setAutofixPausedMutation(),
    onSuccess: (settings) => {
      toast.success(settings.paused ? 'Autofix paused' : 'Autofix resumed')
      invalidate()
    },
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not change the pause')),
  })

  const settings = capability.data?.settings
  // The project the form's current baseline was loaded from.
  const baselineProject = useRef<number | null>(null)
  // Every refetch (after a pause toggle, a save, a window refocus) brings a new
  // `settings` object. Take the server's values as the new baseline, but keep
  // whatever the user has changed and not saved yet -- only for the same
  // project. Unsaved edits made for one project must never ride along into
  // another project's settings, where Save would write them.
  useEffect(() => {
    if (!settings) return
    const sameProject = baselineProject.current === settings.project_id
    baselineProject.current = settings.project_id
    form.reset(toForm(settings), { keepDirtyValues: sameProject })
  }, [settings, form])

  const save = (values: FormValues) =>
    update.mutate({
      path: { project_id: projectId },
      body: {
        mode: values.mode,
        min_confidence: values.minConfidence,
        protected_paths: values.protectedPaths
          .split('\n')
          .map((line) => line.trim())
          .filter(Boolean),
        test_command: values.testCommand.trim(),
      },
    })

  const cap = capability.data
  const mode = useWatch({ control: form.control, name: 'mode' })
  const testCommand = useWatch({ control: form.control, name: 'testCommand' })
  return (
    <div className="space-y-4">
      {capability.isPending && (
        <div className="space-y-2">
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-24 w-full" />
        </div>
      )}
      {capability.isError && (
        <p className="text-sm text-destructive">
          {getErrorMessage(capability.error, 'Could not load autofix settings')}
        </p>
      )}
      {cap && (
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">
            Autofix reproduces a production crash in a sandbox built from the
            deployment that threw it, fixes it in a chat in this project&apos;s
            workspace, and scores the fix from the server&apos;s own test runs.{' '}
            {deliveryLabel(cap.delivery)}
            {cap.delivery_reason && ` ${cap.delivery_reason}.`}
          </p>
          {!cap.configured && (
            <p className="rounded-lg border p-3 text-sm">
              {cap.reason}.{' '}
              {cap.setup_path && (
                <Link className="underline" to={cap.setup_path}>
                  Set it up
                </Link>
              )}
            </p>
          )}
          {showPause && (
            <div className="flex flex-row items-center justify-between rounded-lg border p-4">
              <div className="space-y-0.5 pr-4">
                <Label className="text-base">Pause autofix</Label>
                <p className="text-sm text-muted-foreground">
                  Queued incidents wait and no new ones start until you resume.
                </p>
              </div>
              <Switch
                aria-label="Pause autofix"
                checked={cap.settings.paused}
                disabled={pause.isPending}
                onCheckedChange={(paused) =>
                  pause.mutate({
                    path: { project_id: projectId },
                    body: { paused },
                  })
                }
              />
            </div>
          )}
          <Form {...form}>
            <form onSubmit={form.handleSubmit(save)} className="space-y-4">
              <FormField
                control={form.control}
                name="mode"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>When to fix</FormLabel>
                    <FormControl>
                      <RadioGroup
                        value={field.value}
                        onValueChange={field.onChange}
                        className="grid grid-cols-1 gap-2 md:grid-cols-3"
                      >
                        {MODES.map((mode) => (
                          <Label
                            key={mode.value}
                            className="flex cursor-pointer items-start gap-3 rounded-lg border p-3 font-normal has-[:checked]:border-primary"
                          >
                            <RadioGroupItem value={mode.value} />
                            <span className="space-y-1">
                              <span className="block font-medium">
                                {mode.label}
                              </span>
                              <span className="block text-xs text-muted-foreground">
                                {mode.detail}
                              </span>
                            </span>
                          </Label>
                        ))}
                      </RadioGroup>
                    </FormControl>
                    <FormDescription>
                      Automatic incidents act as whoever saves these settings
                      {cap.settings.owner_user_id != null &&
                        ` (now user ${cap.settings.owner_user_id})`}
                      .
                    </FormDescription>
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="testCommand"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Test command</FormLabel>
                    <FormControl>
                      <Input placeholder="npm test" {...field} />
                    </FormControl>
                    <FormDescription>
                      Run by the server after every fix, besides the
                      agent&apos;s own reproduction. Needed for a fix to reach
                      4/{MAX_CONFIDENCE}.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
              {/* The threshold only decides merging, so it appears with the
                  mode that merges. The value is kept while it is hidden. */}
              {mode === 'auto_merge' && (
                <FormField
                  control={form.control}
                  name="minConfidence"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Merge a fix once it has passed</FormLabel>
                      <FormControl>
                        <RadioGroup
                          value={String(field.value)}
                          onValueChange={(value) =>
                            field.onChange(Number(value))
                          }
                          className="gap-0 divide-y rounded-lg border"
                        >
                          {CONFIDENCE_LEVELS.map((entry) => (
                            <ThresholdLevel
                              key={entry.level}
                              level={entry.level}
                              means={entry.means}
                              threshold={field.value}
                              note={levelNote(entry.level, testCommand)}
                            />
                          ))}
                        </RadioGroup>
                      </FormControl>
                      <FormDescription>
                        Checks build on each other: picking a level requires it
                        and every check above it.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              )}
              <FormField
                control={form.control}
                name="protectedPaths"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Protected paths</FormLabel>
                    <FormControl>
                      <Textarea
                        rows={3}
                        placeholder={'migrations/**\nsrc/billing/**'}
                        {...field}
                      />
                    </FormControl>
                    <FormDescription>
                      One glob per line. A fix that touches one is never merged
                      by policy.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <Button type="submit" disabled={update.isPending}>
                Save
              </Button>
            </form>
          </Form>
        </div>
      )}
    </div>
  )
}

/** Why a level cannot be reached yet, if it cannot. */
function levelNote(level: number, testCommand: string): string | undefined {
  if (level === 4 && !testCommand.trim()) return 'Needs a test command'
  if (level > COLLECTED_UP_TO) return 'Not checked yet'
  return undefined
}

/** The highest level the server collects evidence for today. */
const COLLECTED_UP_TO = 4

/**
 * One rung of the merge threshold: a radio row whose check icon shows whether
 * a fix must pass it at the chosen threshold.
 */
function ThresholdLevel({
  level,
  means,
  threshold,
  note,
}: {
  level: number
  means: string
  threshold: number
  note?: string
}) {
  const required = level <= threshold
  return (
    <Label className="flex cursor-pointer items-center gap-3 px-3 py-2.5 font-normal has-[:checked]:bg-muted/60">
      <RadioGroupItem value={String(level)} />
      <span className="w-6 shrink-0 text-xs tabular-nums text-muted-foreground">
        {level}/{MAX_CONFIDENCE}
      </span>
      <span
        className={cn(
          'min-w-0 flex-1 text-sm',
          !required && 'text-muted-foreground'
        )}
      >
        {means}
      </span>
      {note && (
        <span className="shrink-0 text-xs text-muted-foreground">{note}</span>
      )}
      {required ? (
        <Check aria-hidden className="size-4 shrink-0 text-success" />
      ) : (
        <Minus
          aria-hidden
          className="size-4 shrink-0 text-muted-foreground/50"
        />
      )}
    </Label>
  )
}
