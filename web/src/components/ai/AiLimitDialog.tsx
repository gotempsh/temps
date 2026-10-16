// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { zodResolver } from '@hookform/resolvers/zod'
import { Loader2 } from 'lucide-react'
import { type ReactNode, useEffect } from 'react'
import { useForm } from 'react-hook-form'
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
  type LimitFormValues,
  type LimitPayload,
  limitFormSchema,
  parseLimitForm,
} from '@/lib/ai-gateway-limits'

interface AiLimitDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description: ReactNode
  /** Initial values; the form resets to these every time the dialog opens. */
  initialValues: LimitFormValues
  /** Where a blank field falls back to, e.g. "the instance default". */
  inheritsFrom: string
  submitLabel: string
  pending: boolean
  /** Extra controls rendered above the limit fields (e.g. a project picker). */
  children?: ReactNode
  submitDisabled?: boolean
  onSubmit: (payload: LimitPayload) => void
}

/**
 * Edit form for one scope's limits. A blank field means "inherit" and 0 means
 * "unlimited"; the distinction is spelled out under each input because the
 * two are easy to confuse and have opposite effects.
 */
export function AiLimitDialog({
  open,
  onOpenChange,
  title,
  description,
  initialValues,
  inheritsFrom,
  submitLabel,
  pending,
  children,
  submitDisabled = false,
  onSubmit,
}: AiLimitDialogProps) {
  const form = useForm<LimitFormValues>({
    resolver: zodResolver(limitFormSchema),
    defaultValues: initialValues,
  })

  // Key on the field values, not the object: callers build `initialValues`
  // inline, and resetting on every render would wipe what the user is typing.
  const { requestsPerMinute, monthlyBudgetUsd } = initialValues
  useEffect(() => {
    if (open) form.reset({ requestsPerMinute, monthlyBudgetUsd })
  }, [open, requestsPerMinute, monthlyBudgetUsd, form])

  const submit = (values: LimitFormValues) => {
    const parsed = parseLimitForm(values)
    if (parsed.ok) onSubmit(parsed.payload)
  }

  return (
    <Dialog open={open} onOpenChange={(next) => !pending && onOpenChange(next)}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form onSubmit={form.handleSubmit(submit)} className="space-y-4">
            {children}
            <FormField
              control={form.control}
              name="requestsPerMinute"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Requests per minute</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      inputMode="numeric"
                      placeholder={`Inherit from ${inheritsFrom}`}
                    />
                  </FormControl>
                  <FormDescription>
                    Leave blank to inherit from {inheritsFrom}. Enter 0 for
                    unlimited.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="monthlyBudgetUsd"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Monthly budget (USD)</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      inputMode="decimal"
                      placeholder={`Inherit from ${inheritsFrom}`}
                    />
                  </FormControl>
                  <FormDescription>
                    Caps operator-funded spend per calendar month (UTC).
                    Requests using a caller-supplied provider key are not
                    counted. Blank inherits; 0 is unlimited.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                disabled={pending}
                onClick={() => onOpenChange(false)}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={pending || submitDisabled}>
                {pending && <Loader2 className="mr-2 size-4 animate-spin" />}
                {submitLabel}
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
