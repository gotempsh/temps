// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { Loader2, Save, Timer } from 'lucide-react'

import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'

/** Mirrors the server's clamp — a value outside this is rejected there anyway. */
const MIN_TIMEOUT_SECS = 30
const MAX_TIMEOUT_SECS = 3600
const DEFAULT_TIMEOUT_SECS = 900

interface FormData {
  turn_timeout_secs: number
}

/** "15m 0s" — seconds is the stored unit, minutes is how people think. */
function humanize(seconds: number): string {
  const m = Math.floor(seconds / 60)
  const s = seconds % 60
  if (m === 0) return `${s}s`
  return s === 0 ? `${m}m` : `${m}m ${s}s`
}

/**
 * How long a single AI chat turn may run.
 *
 * Lives on the AI Providers page rather than a page of its own because the
 * right value is a property of the model configured just above it: a turn
 * against a slow self-hosted model can legitimately take ten minutes, while a
 * hosted one finishes in seconds and a tighter ceiling keeps costs predictable.
 */
export function AiChatLimitsCard() {
  const { data: settings, isLoading } = useSettings()
  const updateSettings = useUpdateSettings()

  const {
    register,
    handleSubmit,
    watch,
    reset,
    formState: { isDirty, isSubmitting, errors },
  } = useForm<FormData>({
    defaultValues: { turn_timeout_secs: DEFAULT_TIMEOUT_SECS },
  })

  useEffect(() => {
    if (settings?.ai_chat_limits) {
      reset({
        turn_timeout_secs:
          settings.ai_chat_limits.turn_timeout_secs ?? DEFAULT_TIMEOUT_SECS,
      })
    }
  }, [settings, reset])

  const current = watch('turn_timeout_secs')

  const onSubmit = async (data: FormData) => {
    try {
      await updateSettings.mutateAsync({
        ai_chat_limits: { turn_timeout_secs: Number(data.turn_timeout_secs) },
      })
      reset(data)
      // Say when it takes effect. The setting is read at the start of each
      // turn, so it applies to the next message rather than needing a restart —
      // which is the whole reason it is a stored setting and not a constant.
      toast.success('Saved — applies to the next message')
    } catch {
      toast.error('Failed to save the chat limit')
    }
  }

  if (isLoading) {
    return <Skeleton className="h-48 w-full rounded-lg" />
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <Timer className="h-4 w-4 text-muted-foreground" />
          Chat turn limit
        </CardTitle>
        <CardDescription>
          A turn is bounded by time, not by a number of steps, so the assistant
          can work a problem for as long as it needs rather than stopping after
          a fixed number of tool calls. This is the ceiling for one message: it
          stops an unattended turn, and caps what a single message can cost.
          You can always stop a turn yourself from the chat.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {/* `noValidate` hands validation to react-hook-form. Without it the
            browser's own constraint check fires first and blocks submit with a
            native tooltip — safe, but styled by the browser and phrased by it,
            and it means the messages below could never appear. */}
        <form
          noValidate
          onSubmit={handleSubmit(onSubmit)}
          className="space-y-4"
        >
          <div className="space-y-2">
            <Label htmlFor="turn_timeout_secs">Timeout (seconds)</Label>
            <div className="flex items-center gap-3">
              <Input
                id="turn_timeout_secs"
                type="number"
                className="w-40"
                min={MIN_TIMEOUT_SECS}
                max={MAX_TIMEOUT_SECS}
                {...register('turn_timeout_secs', {
                  valueAsNumber: true,
                  required: 'A timeout is required',
                  min: {
                    value: MIN_TIMEOUT_SECS,
                    message: `Must be at least ${MIN_TIMEOUT_SECS}s — below that a turn cannot finish even simple work`,
                  },
                  max: {
                    value: MAX_TIMEOUT_SECS,
                    message: `Must be at most ${MAX_TIMEOUT_SECS}s (1 hour)`,
                  },
                })}
              />
              {Number.isFinite(current) && (
                <span className="text-sm text-muted-foreground">
                  {humanize(Number(current))}
                </span>
              )}
            </div>
            {errors.turn_timeout_secs && (
              <p className="text-sm text-destructive">
                {errors.turn_timeout_secs.message}
              </p>
            )}
            <p className="text-xs text-muted-foreground">
              A full &ldquo;suggest alerts&rdquo; turn takes about 10 minutes
              against a slow self-hosted model, and seconds against a hosted
              one. Default is {humanize(DEFAULT_TIMEOUT_SECS)}. Checked between
              steps rather than mid-call, so a turn can run over by up to one
              model round instead of being cut off mid-sentence.
            </p>
          </div>

          <Button type="submit" size="sm" disabled={!isDirty || isSubmitting}>
            {isSubmitting ? (
              <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
            ) : (
              <Save className="mr-1.5 h-4 w-4" />
            )}
            Save
          </Button>
        </form>
      </CardContent>
    </Card>
  )
}
