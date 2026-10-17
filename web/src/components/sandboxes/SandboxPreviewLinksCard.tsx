// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link } from 'react-router'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { Link2, Loader2 } from 'lucide-react'
import { toast } from 'sonner'
import { sandboxCreatePreviewLinkMutation } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
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
import { problemDetail } from '@/lib/api-problem'
import {
  DEFAULT_PREVIEW_LINK_TTL_SECONDS,
  PREVIEW_LINK_TTLS,
  previewLinkBlockedReason,
  previewLinkExpiry,
  previewLinkFormSchema,
  toPreviewLinkBody,
  type PreviewLinkFormValues,
} from './sandbox-preview-links'

/** Where the preview password card is anchored on the detail page. */
export const PREVIEW_ACCESS_ANCHOR = 'sandbox-preview-access'

/** Links kept in view for this page session; the API does not list them. */
const MAX_LINKS_SHOWN = 8

interface CreatedLink {
  url: string
  port: number
  expiresAt: number
}

interface SandboxPreviewLinksCardProps {
  sandboxId: string
  hasPassword: boolean
  running: boolean
  destroyed: boolean
  /** Preview URLs are configured on this install. */
  configured: boolean
  /** The sandbox runs on a worker node (no preview URLs there yet). */
  onWorkerNode: boolean
  /** Port worth offering as a starting point. */
  suggestedPort?: number
}

/**
 * Shareable preview links (`POST /v1/sandboxes/{id}/preview-link`).
 *
 * A link is a short-lived credential for one port. The server keeps no list
 * of them and cannot revoke one on its own; rotating the preview password is
 * what invalidates every link at once, so that is what this card points to.
 */
export function SandboxPreviewLinksCard({
  sandboxId,
  hasPassword,
  running,
  destroyed,
  configured,
  onWorkerNode,
  suggestedPort,
}: SandboxPreviewLinksCardProps) {
  const [links, setLinks] = useState<CreatedLink[]>([])
  const blocked = previewLinkBlockedReason({
    configured,
    onWorkerNode,
    hasPassword,
    destroyed,
    running,
  })

  const form = useForm<PreviewLinkFormValues>({
    resolver: zodResolver(previewLinkFormSchema),
    defaultValues: {
      port: suggestedPort ? String(suggestedPort) : '3000',
      path: '',
      ttlSeconds: String(DEFAULT_PREVIEW_LINK_TTL_SECONDS),
    },
  })

  const create = useMutation({
    ...sandboxCreatePreviewLinkMutation(),
    meta: { errorTitle: 'Failed to create preview link' },
    onSuccess: (response, variables) => {
      setLinks((current) =>
        [
          {
            url: response.url,
            port: variables.body.port,
            expiresAt: response.expires_at,
          },
          ...current,
        ].slice(0, MAX_LINKS_SHOWN)
      )
      toast.success('Preview link created')
    },
  })

  const submit = (values: PreviewLinkFormValues) =>
    create.mutate({
      path: { id: sandboxId },
      body: toPreviewLinkBody(values),
    })

  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          <Link2 className="h-4 w-4" />
          Share a preview
        </CardTitle>
        <CardDescription>
          Create a link that opens one port of this sandbox without the
          password, for a limited time.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {blocked && (
          <Alert>
            <AlertTitle>Preview links unavailable</AlertTitle>
            <AlertDescription>
              {blocked}
              {!configured && !onWorkerNode && !destroyed && (
                <>
                  {' '}
                  <Link
                    to="/settings"
                    className="font-medium underline underline-offset-2"
                  >
                    Open platform settings
                  </Link>
                </>
              )}
              {configured && !onWorkerNode && !hasPassword && !destroyed && (
                <>
                  {' '}
                  <a
                    href={`#${PREVIEW_ACCESS_ANCHOR}`}
                    className="font-medium underline underline-offset-2"
                  >
                    Go to preview access
                  </a>
                </>
              )}
            </AlertDescription>
          </Alert>
        )}

        <Form {...form}>
          <form
            onSubmit={form.handleSubmit(submit)}
            className="grid gap-3 sm:grid-cols-[110px_1fr_160px_auto] sm:items-start"
            noValidate
          >
            <FormField
              control={form.control}
              name="port"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Port</FormLabel>
                  <FormControl>
                    <Input
                      inputMode="numeric"
                      disabled={blocked !== null}
                      {...field}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="path"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Landing path</FormLabel>
                  <FormControl>
                    <Input
                      placeholder="/"
                      className="font-mono"
                      autoComplete="off"
                      disabled={blocked !== null}
                      {...field}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="ttlSeconds"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Valid for</FormLabel>
                  <Select
                    value={field.value}
                    onValueChange={field.onChange}
                    disabled={blocked !== null}
                  >
                    <FormControl>
                      <SelectTrigger>
                        <SelectValue />
                      </SelectTrigger>
                    </FormControl>
                    <SelectContent>
                      {PREVIEW_LINK_TTLS.map((option) => (
                        <SelectItem
                          key={option.seconds}
                          value={String(option.seconds)}
                        >
                          {option.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <FormMessage />
                </FormItem>
              )}
            />
            <div className="sm:pt-[26px]">
              <Button
                type="submit"
                disabled={blocked !== null || create.isPending}
              >
                {create.isPending && (
                  <Loader2 className="mr-2 size-4 animate-spin" />
                )}
                Create link
              </Button>
            </div>
          </form>
        </Form>

        {create.isError && (
          <Alert variant="destructive">
            <AlertDescription>
              {problemDetail(
                create.error,
                'The preview link could not be created.'
              )}
            </AlertDescription>
          </Alert>
        )}

        {links.length > 0 && (
          <ul className="space-y-2" aria-label="Links created in this session">
            {links.map((link) => (
              <li
                key={`${link.expiresAt}:${link.url}`}
                className="flex items-center gap-2 rounded-md border bg-muted/40 px-3 py-2"
              >
                <div className="min-w-0 flex-1">
                  <p className="truncate font-mono text-xs">{link.url}</p>
                  <p className="text-xs text-muted-foreground">
                    Port {link.port} · {previewLinkExpiry(link.expiresAt)}
                  </p>
                </div>
                <CopyButton
                  value={link.url}
                  minimal
                  label={`Copy preview link for port ${link.port}`}
                  className="shrink-0"
                />
              </li>
            ))}
          </ul>
        )}

        <p className="text-xs text-muted-foreground">
          Anyone holding a link can open the preview until it expires, and the
          link itself is the credential. Individual links cannot be revoked and
          are not listed by the server; to invalidate every link at once, rotate
          the preview password under{' '}
          <a
            href={`#${PREVIEW_ACCESS_ANCHOR}`}
            className="font-medium underline underline-offset-2"
          >
            preview access
          </a>
          .
        </p>
      </CardContent>
    </Card>
  )
}
