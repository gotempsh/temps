// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { setStatusPagePasswordMutation } from '@/api/client/@tanstack/react-query.gen'
import type { StatusPageResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import {
  Form,
  FormControl,
  FormField,
  FormDescription,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Lock } from 'lucide-react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import {
  MAX_PAGE_PASSWORD_CHARS,
  pagePasswordSchema,
  type PagePasswordValues,
} from './notifications'
import { operationQueries } from './status-page-form'

/** Password protection of a status page (ADR 108 D2). */
export function StatusPagePassword({ page }: { page: StatusPageResponse }) {
  const queryClient = useQueryClient()
  const form = useForm<PagePasswordValues>({
    resolver: zodResolver(pagePasswordSchema),
    defaultValues: { password: '' },
  })
  const save = useMutation({
    ...setStatusPagePasswordMutation(),
    meta: { errorTitle: 'Failed to change the page password' },
    onSuccess: async (saved) => {
      form.reset({ password: '' })
      await queryClient.invalidateQueries(operationQueries('getStatusPage'))
      await queryClient.invalidateQueries(operationQueries('listStatusPages'))
      toast.success(
        saved.has_password
          ? 'Visitors now need the password. Everyone signed in was signed out.'
          : 'The page is open to anyone with its link again.'
      )
    },
  })
  const path = { status_page_id: page.id }

  return (
    <Card>
      <CardContent className="space-y-4 p-4">
        <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
          <div className="space-y-1">
            <h3 className="flex items-center gap-2 font-semibold">
              <Lock className="size-4" />
              Password
            </h3>
            <p className="text-sm text-muted-foreground">
              Ask visitors for a password before they see the page.
            </p>
          </div>
          <Badge
            variant={page.has_password ? 'success-subtle' : 'neutral-subtle'}
          >
            {page.has_password ? 'Protected' : 'Open'}
          </Badge>
        </div>
        <Form {...form}>
          <form
            className="flex flex-col gap-3 sm:flex-row sm:items-start"
            onSubmit={form.handleSubmit((values) =>
              save.mutate({ path, body: { password: values.password } })
            )}
          >
            <FormField
              control={form.control}
              name="password"
              render={({ field }) => (
                <FormItem className="w-full sm:max-w-sm">
                  <FormLabel className="sr-only">Page password</FormLabel>
                  <FormControl>
                    <Input
                      type="password"
                      autoComplete="new-password"
                      maxLength={MAX_PAGE_PASSWORD_CHARS}
                      placeholder={
                        page.has_password ? 'New password' : 'Password'
                      }
                      {...field}
                    />
                  </FormControl>
                  <FormDescription>
                    A protected page has no feed, and its JSON is not shared
                    with other sites.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />
            <Button type="submit" variant="outline" disabled={save.isPending}>
              {page.has_password ? 'Change password' : 'Protect page'}
            </Button>
            {page.has_password && (
              <Button
                type="button"
                variant="ghost"
                disabled={save.isPending}
                onClick={() => save.mutate({ path, body: { password: null } })}
              >
                Remove password
              </Button>
            )}
          </form>
        </Form>
      </CardContent>
    </Card>
  )
}
