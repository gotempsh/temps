// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Identify signed-in users (ADR 109 D4): how to call identify, and whether
 * the browser may do it or only the app's backend.
 */

import {
  getAnalyticsIdentitySettingsOptions,
  updateAnalyticsIdentitySettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client/types.gen'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CodeBlock } from '@/components/ui/code-block'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { problemDetail } from '@/lib/api-problem'
import { identifySnippets } from '@/lib/visitor-identity'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { UserCheck } from 'lucide-react'
import { toast } from 'sonner'

export function IdentifyUsersCard({ project }: { project: ProjectResponse }) {
  const queryClient = useQueryClient()
  const options = getAnalyticsIdentitySettingsOptions({
    path: { project_id: project.id },
  })
  const settings = useQuery(options)
  const update = useMutation({
    ...updateAnalyticsIdentitySettingsMutation(),
    onSuccess: async (result) => {
      toast.success(
        result.accept_browser_identify
          ? 'identify() from the browser is applied again.'
          : 'identify() from the browser is ignored; only your backend can identify visitors.'
      )
      await queryClient.invalidateQueries({ queryKey: options.queryKey })
    },
    onError: (error) =>
      toast.error(
        problemDetail(error, 'Could not change the browser identify setting')
      ),
  })

  const snippets = identifySnippets()
  const cookieless = settings.data?.mode === 'cookieless'
  const accept = settings.data?.accept_browser_identify ?? true

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <UserCheck className="h-4 w-4" aria-hidden="true" />
          Identify signed-in users
        </CardTitle>
        <CardDescription>
          Attach your app&apos;s user id to the visitor at signup or sign-in, so
          the visit that led to it, and the same person on other devices, show
          up under one user.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {settings.isLoading ? (
          <Skeleton className="h-16 w-full" />
        ) : settings.isError ? (
          <p className="text-sm text-muted-foreground">
            {problemDetail(
              settings.error,
              'The identify settings could not be loaded.'
            )}
          </p>
        ) : cookieless ? (
          <p
            className="rounded-md border p-3 text-sm text-muted-foreground"
            role="note"
          >
            This project tracks visitors cookieless, so identify stores nothing,
            from the browser or from your backend: a user id on a daily visitor
            would link its days together. Switch to &quot;Recognise returning
            visitors&quot; above to use it.
          </p>
        ) : (
          <div className="flex items-start justify-between gap-4 rounded-md border p-3">
            <div className="space-y-1">
              <label
                htmlFor="accept-browser-identify"
                className="text-sm font-medium"
              >
                Accept identify from the browser
              </label>
              <p className="text-xs text-muted-foreground">
                {accept
                  ? 'A page can label its own visitor. The id is a claim by that browser and is shown as such.'
                  : 'Ignored: only your backend, with its deployment token, can identify a visitor.'}{' '}
                An identity your backend set is never replaced from the browser.
              </p>
            </div>
            <Switch
              id="accept-browser-identify"
              checked={accept}
              disabled={update.isPending}
              onCheckedChange={(checked) =>
                update.mutate({
                  path: { project_id: project.id },
                  body: { accept_browser_identify: checked },
                })
              }
            />
          </div>
        )}

        <div className="space-y-2 text-sm">
          <p className="text-muted-foreground">
            From the page (React, or the script tag):
          </p>
          <CodeBlock language="tsx" code={snippets.react} showCopy />
          <CodeBlock language="javascript" code={snippets.script} showCopy />
          <p className="text-muted-foreground">
            From your backend on Temps, which needs no configuration: the
            visitor comes from the request&apos;s cookie, and the deployment
            token is injected.
          </p>
          <CodeBlock language="typescript" code={snippets.node} showCopy />
        </div>
      </CardContent>
    </Card>
  )
}
