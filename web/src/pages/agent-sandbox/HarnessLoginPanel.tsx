// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Fragment, useEffect, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { toast } from 'sonner'
import { AlertTriangle, ExternalLink, Loader2 } from 'lucide-react'

import {
  cancelAiProviderLogin,
  getAiProviderLogin,
  startAiProviderLogin,
  submitAiProviderLoginCode,
  type ProviderCatalogDto,
} from '@/api/client'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
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
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { problemDetail } from '@/lib/api-problem'
import {
  formatElapsed,
  isLoginActive,
  isLoginTerminal,
  isSignInConflict,
  loginButtonLabel,
  loginCodeSchema,
  loginPhaseTitle,
  type LoginCodeFormValues,
} from './harness-login'

/** Render `code` spans from server hints such as "Install it with `…`". */
function InlineCode({ text }: { text: string }) {
  return (
    <>
      {text.split('`').map((part, index) =>
        index % 2 === 1 ? (
          <code
            key={index}
            className="rounded bg-muted px-1 py-0.5 font-mono text-[0.85em]"
          >
            {part}
          </code>
        ) : (
          <Fragment key={index}>{part}</Fragment>
        )
      )}
    </>
  )
}

const POLL_INTERVAL_MS = 1000

/**
 * One-click subscription sign-in. The server runs the provider's own CLI
 * sign-in; this panel shows the link (and device code), takes the pasted code
 * for providers that need one, and reports live progress.
 *
 * The button is always present for providers that support it. When the server
 * cannot start a sign-in yet, the reason and the fix are shown next to it and
 * pressing it still reports the exact problem rather than doing nothing.
 */
export function HarnessLoginPanel({
  provider,
  onSucceeded,
  disabled = false,
}: {
  provider: ProviderCatalogDto
  /** Called once when the sign-in finished and was saved. */
  onSucceeded: () => Promise<void> | void
  disabled?: boolean
}) {
  const queryClient = useQueryClient()
  const providerId = provider.id
  const queryKey = ['ai-provider-login', providerId] as const
  const [open, setOpen] = useState(false)
  const [startError, setStartError] = useState<string | null>(null)
  const announcedSession = useRef<string | null>(null)
  // The first session this panel sees may be an old, finished one (the server
  // keeps it, and so does the query cache). Only a sign-in that finishes while
  // the panel is mounted is announced, otherwise reopening this panel to
  // replace a credential would report the previous success again.
  const baselineTaken = useRef(false)
  const codeForm = useForm<LoginCodeFormValues>({
    resolver: zodResolver(loginCodeSchema),
    defaultValues: { code: '' },
  })

  const sessionQuery = useQuery({
    queryKey,
    queryFn: async () => {
      const { data } = await getAiProviderLogin({
        path: { provider_id: providerId },
        throwOnError: true,
      })
      return data
    },
    // Always look for an existing sign-in on mount: a reload or tab switch
    // while one is running must offer to resume it rather than start a second
    // one (the server refuses with 409). A 404 simply means none yet. Polling
    // continues only while the sign-in is active.
    retry: false,
    refetchOnWindowFocus: false,
    refetchInterval: (query) =>
      isLoginActive(query.state.data?.phase) ? POLL_INTERVAL_MS : false,
  })
  const session = sessionQuery.data

  const startMutation = useMutation({
    mutationFn: async () => {
      const { data } = await startAiProviderLogin({
        path: { provider_id: providerId },
        throwOnError: true,
      })
      return data
    },
    onMutate: () => setStartError(null),
    onSuccess: (data) => {
      queryClient.setQueryData(queryKey, data)
      codeForm.reset({ code: '' })
      setOpen(true)
    },
    onError: (error) => {
      // The server already has a sign-in for this provider (started from
      // another tab or before a reload): show it instead of failing.
      if (isSignInConflict(error)) {
        void queryClient.invalidateQueries({ queryKey })
        setOpen(true)
        return
      }
      const detail = problemDetail(
        error,
        'Could not start the sign-in. Check that the CLI is installed on this server.'
      )
      setStartError(detail)
      toast.error(`Could not start ${provider.name} sign-in`, {
        description: detail,
      })
    },
  })

  const submitMutation = useMutation({
    mutationFn: async (value: string) => {
      const { data } = await submitAiProviderLoginCode({
        path: { provider_id: providerId },
        body: { code: value },
        throwOnError: true,
      })
      return data
    },
    onSuccess: (data) => {
      queryClient.setQueryData(queryKey, data)
      codeForm.reset({ code: '' })
    },
    onError: (error) => {
      toast.error('Could not submit the code', {
        description: problemDetail(
          error,
          'The server did not accept the code. Copy it again and retry.'
        ),
      })
    },
  })

  const cancelMutation = useMutation({
    mutationFn: async () => {
      const { data } = await cancelAiProviderLogin({
        path: { provider_id: providerId },
        throwOnError: true,
      })
      return data
    },
    onSuccess: (data) => queryClient.setQueryData(queryKey, data),
    onError: (error) => {
      toast.error('Could not cancel the sign-in', {
        description: problemDetail(error, 'Try again in a moment.'),
      })
    },
  })

  // Publish a finished sign-in exactly once per session.
  const phase = session?.phase
  const sessionId = session?.session_id
  useEffect(() => {
    if (!phase || !sessionId) return
    if (!baselineTaken.current) {
      baselineTaken.current = true
      if (isLoginTerminal(phase)) {
        announcedSession.current = sessionId
        return
      }
    }
    if (phase !== 'succeeded') return
    if (announcedSession.current === sessionId) return
    announcedSession.current = sessionId
    toast.success(`${provider.name} signed in`)
    void Promise.resolve(onSucceeded())
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [phase, sessionId])

  const working = startMutation.isPending
  const active = isLoginActive(phase)
  const busy = disabled || working
  const requiresCode = Boolean(session?.requires_code)
  const submitted = submitMutation.isPending

  const activeSessionHint =
    active && !open ? (
      <p className="text-xs text-muted-foreground">
        A sign-in is in progress ({formatElapsed(session?.elapsed_seconds ?? 0)}
        ).
      </p>
    ) : null

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        Sign in with the {provider.name} account you already pay for. Temps runs
        the CLI&apos;s own sign-in on this server; you approve access in your
        browser and no API key is needed.
      </p>

      {provider.login_unavailable_reason && (
        <div
          role="status"
          className="flex items-start gap-2 rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-amber-800 dark:text-amber-200"
        >
          <AlertTriangle
            className="mt-0.5 size-3.5 shrink-0"
            aria-hidden="true"
          />
          <span>
            <InlineCode text={provider.login_unavailable_reason} />
          </span>
        </div>
      )}

      <div className="flex flex-wrap items-center gap-3">
        <Button
          type="button"
          size="sm"
          disabled={busy}
          onClick={() => {
            if (active) {
              setOpen(true)
              return
            }
            startMutation.mutate()
          }}
        >
          {working && <Loader2 className="mr-1.5 size-4 animate-spin" />}
          {active ? 'Resume sign-in' : loginButtonLabel(provider.name)}
        </Button>
        {activeSessionHint}
      </div>
      {startError && !open && (
        <p role="alert" className="text-sm break-words text-destructive">
          {startError}
        </p>
      )}

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>
              {session
                ? loginPhaseTitle(session.phase, provider.name)
                : `Sign in to ${provider.name}`}
            </DialogTitle>
            <DialogDescription>
              {session?.detail ||
                'Approve access in your browser. This window updates by itself.'}
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4" aria-live="polite">
            {!session && sessionQuery.isPending && (
              <div className="space-y-3" aria-busy="true">
                <Skeleton className="h-4 w-40" />
                <Skeleton className="h-8 w-56" />
                <Skeleton className="h-4 w-3/4" />
              </div>
            )}

            {session?.phase === 'starting' && (
              <p className="flex items-center gap-2 text-sm text-muted-foreground">
                <Loader2 className="size-4 animate-spin" />
                Waiting for {provider.name} to offer a sign-in link…
              </p>
            )}

            {session?.verification_url && isLoginActive(session.phase) && (
              <div className="space-y-3">
                <div className="space-y-1.5">
                  <Label>1. Open the sign-in page</Label>
                  <div className="flex flex-wrap items-center gap-2">
                    <Button asChild size="sm" variant="outline">
                      <a
                        href={session.verification_url}
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        <ExternalLink className="mr-1.5 size-3.5" />
                        Open sign-in page
                      </a>
                    </Button>
                    <CopyButton
                      value={session.verification_url}
                      label="Copy sign-in link"
                      variant="outline"
                      size="sm"
                    >
                      Copy link
                    </CopyButton>
                  </div>
                </div>

                {session.user_code && (
                  <div className="space-y-1.5">
                    <Label>2. Enter this code</Label>
                    <div className="flex items-center gap-2">
                      <code className="rounded bg-muted px-3 py-1.5 font-mono text-lg tracking-widest">
                        {session.user_code}
                      </code>
                      <CopyButton
                        value={session.user_code}
                        label="Copy device code"
                        variant="outline"
                        size="sm"
                      />
                    </div>
                  </div>
                )}

                {requiresCode && session.phase === 'waiting' && (
                  <Form {...codeForm}>
                    <form
                      className="space-y-1.5"
                      onSubmit={codeForm.handleSubmit((values) =>
                        submitMutation.mutate(values.code)
                      )}
                    >
                      <FormField
                        control={codeForm.control}
                        name="code"
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>
                              2. Paste the code shown after approving
                            </FormLabel>
                            <div className="flex gap-2">
                              <FormControl>
                                <Input
                                  {...field}
                                  type="password"
                                  autoComplete="off"
                                  spellCheck={false}
                                  placeholder="Paste code"
                                  disabled={submitted}
                                />
                              </FormControl>
                              <Button
                                type="submit"
                                size="sm"
                                disabled={submitted}
                              >
                                {submitted && (
                                  <Loader2 className="mr-1.5 size-4 animate-spin" />
                                )}
                                Submit
                              </Button>
                            </div>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                    </form>
                  </Form>
                )}

                {!requiresCode && session.phase === 'waiting' && (
                  <p className="flex items-center gap-2 text-sm text-muted-foreground">
                    <Loader2 className="size-4 animate-spin" />
                    Waiting for you to finish in the browser…
                  </p>
                )}
              </div>
            )}

            {session?.phase === 'verifying' && (
              <p className="flex items-center gap-2 text-sm text-muted-foreground">
                <Loader2 className="size-4 animate-spin" />
                Confirming the sign-in with {provider.name}…
              </p>
            )}

            {session && isLoginTerminal(session.phase) && (
              <p
                role={session.phase === 'succeeded' ? 'status' : 'alert'}
                className={
                  session.phase === 'succeeded'
                    ? 'text-sm text-emerald-700 dark:text-emerald-300'
                    : 'text-sm break-words text-destructive'
                }
              >
                {session.detail}
              </p>
            )}
          </div>

          <DialogFooter className="items-center gap-2 sm:justify-between">
            <span className="text-xs text-muted-foreground">
              {session
                ? `Elapsed ${formatElapsed(session.elapsed_seconds)} of ${formatElapsed(session.timeout_seconds)}`
                : ''}
            </span>
            <div className="flex gap-2">
              {active ? (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={cancelMutation.isPending}
                  onClick={() => cancelMutation.mutate()}
                >
                  {cancelMutation.isPending && (
                    <Loader2 className="mr-1.5 size-4 animate-spin" />
                  )}
                  Cancel sign-in
                </Button>
              ) : (
                <>
                  {session &&
                    (session.phase === 'failed' ||
                      session.phase === 'cancelled') && (
                      <Button
                        type="button"
                        size="sm"
                        disabled={working}
                        onClick={() => startMutation.mutate()}
                      >
                        {working && (
                          <Loader2 className="mr-1.5 size-4 animate-spin" />
                        )}
                        Try again
                      </Button>
                    )}
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => setOpen(false)}
                  >
                    Close
                  </Button>
                </>
              )}
            </div>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
