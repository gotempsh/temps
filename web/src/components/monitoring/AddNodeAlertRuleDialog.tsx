// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { NodeCreateAlertRuleRequest } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  NODE_ALERT_COMPARATORS,
  NODE_ALERT_METRIC_LABELS,
  NODE_ALERT_SEVERITIES,
  defaultRuleName,
  nodeAlertRuleFormSchema,
  toCreateRequest,
  type AlertableNodeMetric,
  type NodeAlertRuleFormValues,
} from '@/lib/node-alert-rules'
import { zodResolver } from '@hookform/resolvers/zod'
import { Loader2 } from 'lucide-react'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'

type Props = {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Metrics with no rule yet; the server allows one rule per metric. */
  availableMetrics: AlertableNodeMetric[]
  /** Who the rule is for, in the dialog copy: "the control plane" / "this worker". */
  subject: string
  isPending: boolean
  onSubmit: (body: NodeCreateAlertRuleRequest) => void
}

function formDefaults(metric: AlertableNodeMetric): NodeAlertRuleFormValues {
  return {
    name: defaultRuleName(metric),
    metric_name: metric,
    comparator: '>',
    threshold: metric === 'node.disk_percent' ? 85 : 90,
    severity: 'warning',
    for_minutes: 5,
  }
}

export function AddNodeAlertRuleDialog({
  open,
  onOpenChange,
  availableMetrics,
  subject,
  isPending,
  onSubmit,
}: Props) {
  const firstMetric = availableMetrics[0] ?? 'node.cpu_percent'
  const form = useForm<NodeAlertRuleFormValues>({
    resolver: zodResolver(nodeAlertRuleFormSchema),
    defaultValues: formDefaults(firstMetric),
  })

  // Start each opening from a fresh rule for a metric that is still free.
  useEffect(() => {
    if (open) form.reset(formDefaults(firstMetric))
  }, [open, firstMetric, form])

  const noMetricLeft = availableMetrics.length === 0

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add an alert rule</DialogTitle>
          <DialogDescription>
            Raise an alarm when {subject} stays beyond a usage level. A metric
            has one rule; edit the existing rule to change its level.
          </DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form
            onSubmit={form.handleSubmit((values) =>
              onSubmit(toCreateRequest(values))
            )}
            className="space-y-4"
          >
            <FormField
              control={form.control}
              name="metric_name"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Metric</FormLabel>
                  <Select
                    value={field.value}
                    onValueChange={(value) => {
                      field.onChange(value)
                      // Keep an untouched suggested name in step with the metric.
                      const current = form.getValues('name')
                      const untouched = availableMetrics.some(
                        (m) => defaultRuleName(m) === current
                      )
                      if (untouched) {
                        form.setValue(
                          'name',
                          defaultRuleName(value as AlertableNodeMetric)
                        )
                      }
                    }}
                    disabled={noMetricLeft}
                  >
                    <FormControl>
                      <SelectTrigger>
                        <SelectValue />
                      </SelectTrigger>
                    </FormControl>
                    <SelectContent>
                      {availableMetrics.map((metric) => (
                        <SelectItem key={metric} value={metric}>
                          {NODE_ALERT_METRIC_LABELS[metric]}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  {noMetricLeft && (
                    <FormDescription>
                      CPU, memory and disk already have a rule.
                    </FormDescription>
                  )}
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="name"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Name</FormLabel>
                  <FormControl>
                    <Input {...field} />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />

            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <FormField
                control={form.control}
                name="comparator"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Fires when usage is</FormLabel>
                    <Select value={field.value} onValueChange={field.onChange}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {NODE_ALERT_COMPARATORS.map((c) => (
                          <SelectItem key={c} value={c}>
                            {c}
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
                name="threshold"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Threshold (%)</FormLabel>
                    <FormControl>
                      <Input
                        type="number"
                        inputMode="decimal"
                        min={0}
                        max={100}
                        step="any"
                        value={Number.isNaN(field.value) ? '' : field.value}
                        onChange={(e) =>
                          field.onChange(
                            e.target.value === ''
                              ? Number.NaN
                              : Number(e.target.value)
                          )
                        }
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>

            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <FormField
                control={form.control}
                name="for_minutes"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>For (minutes)</FormLabel>
                    <FormControl>
                      <Input
                        type="number"
                        inputMode="numeric"
                        min={0}
                        max={1440}
                        step={1}
                        value={Number.isNaN(field.value) ? '' : field.value}
                        onChange={(e) =>
                          field.onChange(
                            e.target.value === ''
                              ? Number.NaN
                              : Number(e.target.value)
                          )
                        }
                      />
                    </FormControl>
                    <FormDescription>
                      How long it must persist. 0 fires immediately.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="severity"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Severity</FormLabel>
                    <Select value={field.value} onValueChange={field.onChange}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {NODE_ALERT_SEVERITIES.map((s) => (
                          <SelectItem key={s} value={s}>
                            {s}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>

            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
                disabled={isPending}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={isPending || noMetricLeft}>
                {isPending && <Loader2 className="mr-2 size-4 animate-spin" />}
                Add rule
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
