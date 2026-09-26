'use client'

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { HighlightedCode } from '@/components/ui/code-block'

import { getEmailOptions } from '@/api/client/@tanstack/react-query.gen'
import { client } from '@/api/client/client.gen'
import { EmailResponse } from '@/api/client/types.gen'
import { EmailEventTimeline } from '@/components/email/EmailEventTimeline'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  Button,
  Callout,
  CopyAction,
  Detail,
  PageContainer,
  PageState,
  Status,
  fmtDateTime,
  fmtDuration,
  fmtNumber,
  fmtRelativeTime,
  useUrlState,
  type DetailFact,
  type StatusTone,
} from '@temps-sdk/ds'
import { useQuery } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  Code,
  Eye,
  FileText,
  MousePointerClick,
} from 'lucide-react'
import { useEffect, useRef, useState, type ReactNode } from 'react'
import { Link, useParams } from 'react-router'

// A small, page-local key-value list. Used here for both the custom-headers
// aside panel and the "Message details" card in `main` — genuinely a
// one-off pattern so far (no other migrated detail page needs it yet); per
// the design-system skill's "Adding a primitive" procedure, promote this to
// a package export once a second page actually needs it, not before.
function KeyValueList({ items }: { items: { key: string; value: ReactNode }[] }) {
  if (items.length === 0) {
    return <p className="text-sm text-muted-foreground">Nothing to show.</p>
  }
  return (
    <dl className="divide-y divide-border text-sm">
      {items.map((item) => (
        <div
          key={item.key}
          className="flex flex-col gap-1 py-2 first:pt-0 last:pb-0 sm:flex-row sm:items-start sm:gap-4"
        >
          <dt className="w-28 shrink-0 text-muted-foreground">{item.key}</dt>
          <dd className="min-w-0 flex-1 break-all font-mono text-xs">{item.value}</dd>
        </div>
      ))}
    </dl>
  )
}

function HtmlPreview({ html }: { html: string }) {
  const iframeRef = useRef<HTMLIFrameElement>(null)
  const [iframeHeight, setIframeHeight] = useState(500)

  useEffect(() => {
    if (iframeRef.current) {
      const iframe = iframeRef.current
      const doc = iframe.contentDocument || iframe.contentWindow?.document

      if (doc) {
        // Add base styles for the iframe content
        const styledHtml = `
          <!DOCTYPE html>
          <html>
            <head>
              <meta charset="utf-8">
              <meta name="viewport" content="width=device-width, initial-scale=1">
              <style>
                body {
                  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
                  margin: 0;
                  padding: 16px;
                  background-color: #ffffff;
                  color: #000000;
                }
                img {
                  max-width: 100%;
                  height: auto;
                }
                a {
                  color: #2563eb;
                }
              </style>
            </head>
            <body>
              ${html}
            </body>
          </html>
        `
        doc.open()
        doc.write(styledHtml)
        doc.close()

        // Adjust iframe height based on content
        const resizeObserver = new ResizeObserver(() => {
          if (doc.body) {
            const newHeight = Math.max(
              300,
              Math.min(doc.body.scrollHeight + 40, 800)
            )
            setIframeHeight(newHeight)
          }
        })

        if (doc.body) {
          resizeObserver.observe(doc.body)
        }

        return () => resizeObserver.disconnect()
      }
    }
  }, [html])

  return (
    <div className="border rounded-lg overflow-hidden bg-white">
      <iframe
        ref={iframeRef}
        title="Email HTML Preview"
        className="w-full border-0"
        style={{ height: `${iframeHeight}px` }}
        sandbox="allow-same-origin"
      />
    </div>
  )
}

function TextPreview({ text }: { text: string }) {
  return (
    <div className="border rounded-lg bg-muted/30 p-4">
      <pre className="text-sm whitespace-pre-wrap font-mono break-all">
        {text}
      </pre>
    </div>
  )
}

function SourceView({
  content,
  type,
}: {
  content: string
  type: 'html' | 'text'
}) {
  return (
    <div className="relative">
      <div className="absolute top-2 right-2 z-10">
        <CopyAction
          value={content}
          className="bg-background/80 backdrop-blur-sm"
        />
      </div>
      <div className="border rounded-lg bg-muted/30 p-4 max-h-[600px] overflow-auto">
        <pre className="text-xs font-mono whitespace-pre-wrap break-all">
          <HighlightedCode
            code={type === 'html' ? content : content}
            language={type === 'html' ? 'html' : 'text'}
          />
        </pre>
      </div>
    </div>
  )
}

// Mirrors `StatusBadge` (components/email/shared.tsx) tone-for-tone rather
// than inventing a new severity ordering, then layers the most informative
// tracking signal on top of a "sent" status — an open/click is a stronger,
// more current signal than a bare "sent" for the Detail template's verdict.
const EMAIL_STATUS_VERDICT: Record<string, { tone: StatusTone; label: string }> = {
  sent: { tone: 'ok', label: 'Sent' },
  queued: { tone: 'idle', label: 'Queued' },
  sending: { tone: 'running', label: 'Sending' },
  failed: { tone: 'error', label: 'Failed' },
  captured: { tone: 'idle', label: 'Captured' },
  delivery_unknown: { tone: 'warn', label: 'Delivery unknown' },
}

function emailVerdict(email: EmailResponse): { tone: StatusTone; label: string } {
  if (email.status === 'sent') {
    if (email.track_clicks && email.click_count > 0) {
      return { tone: 'ok', label: 'Clicked' }
    }
    if (email.track_opens && email.open_count > 0) {
      return { tone: 'ok', label: 'Opened' }
    }
  }
  return EMAIL_STATUS_VERDICT[email.status] ?? { tone: 'idle', label: email.status }
}

function emailFacts(email: EmailResponse): DetailFact[] {
  const facts: DetailFact[] = [
    {
      label: 'From',
      value: email.from_name
        ? `${email.from_name} <${email.from_address}>`
        : email.from_address,
    },
    { label: 'To', value: email.to_addresses.join(', ') },
  ]

  const deliveredAt = email.sent_at ?? email.created_at
  facts.push({
    label: email.sent_at ? 'Sent' : 'Created',
    value: fmtDateTime(deliveredAt),
  })

  if (email.sent_at) {
    const tookMs = new Date(email.sent_at).getTime() - new Date(email.created_at).getTime()
    if (tookMs > 0) {
      facts.push({ label: 'Took', value: fmtDuration(tookMs) })
    }
  }

  if (email.track_opens) {
    facts.push({ label: 'Opens', value: fmtNumber(email.open_count) })
  }
  if (email.track_clicks) {
    facts.push({ label: 'Clicks', value: fmtNumber(email.click_count) })
  }

  // The record recipe caps facts at 6 — From/To/Sent/Took/Opens/Clicks is
  // already exactly that ceiling in the fullest case, but stay defensive.
  return facts.slice(0, 6)
}

function EmailDetailContent({ email }: { email: EmailResponse }) {
  const hasHtml = !!email.html_body
  const hasText = !!email.text_body
  const defaultTab = hasHtml ? 'preview' : hasText ? 'text' : undefined

  const { get, patch } = useUrlState<'tab'>()
  const activeTab = get('tab') ?? defaultTab ?? 'preview'

  const { data: trackingLinks } = useQuery({
    queryKey: ['email-tracking-links', email.id],
    queryFn: async () => {
      const res = await client.get<
        { link_index: number; original_url: string; click_count: number }[]
      >({
        url: '/emails/{id}/tracking/links',
        path: { id: email.id },
      })
      return res.data ?? []
    },
    enabled: !!email.track_clicks,
  })

  const messageDetails: { key: string; value: ReactNode }[] = []
  if (email.cc_addresses && email.cc_addresses.length > 0) {
    messageDetails.push({ key: 'CC', value: email.cc_addresses.join(', ') })
  }
  if (email.bcc_addresses && email.bcc_addresses.length > 0) {
    messageDetails.push({ key: 'BCC', value: email.bcc_addresses.join(', ') })
  }
  if (email.reply_to) {
    messageDetails.push({ key: 'Reply-To', value: email.reply_to })
  }
  if (email.tags && email.tags.length > 0) {
    messageDetails.push({
      key: 'Tags',
      value: (
        <div className="flex flex-wrap gap-1 font-sans">
          {email.tags.map((tag) => (
            <Badge key={tag} variant="outline">
              {tag}
            </Badge>
          ))}
        </div>
      ),
    })
  }
  if (email.provider_message_id) {
    messageDetails.push({
      key: 'Provider Message ID',
      value: (
        <span className="inline-flex items-center gap-1">
          <code className="break-all">{email.provider_message_id}</code>
          <CopyAction value={email.provider_message_id} label="Copy provider message ID" />
        </span>
      ),
    })
  }

  const hasTrackingDetail =
    (email.track_opens && email.first_opened_at) ||
    (email.track_clicks && email.first_clicked_at) ||
    (trackingLinks && trackingLinks.length > 0)

  return (
    <>
      {email.error_message ? (
        <Callout tone="error" title="Delivery error">
          {email.error_message}
        </Callout>
      ) : null}

      {hasTrackingDetail ? (
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-base flex items-center gap-2">
              <MousePointerClick className="h-4 w-4" />
              Tracking
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="grid grid-cols-2 gap-4 text-sm">
              {email.track_opens && email.first_opened_at && (
                <div className="space-y-1">
                  <p className="text-xs text-muted-foreground">First opened</p>
                  <p title={fmtDateTime(email.first_opened_at)}>
                    {fmtRelativeTime(email.first_opened_at)}
                  </p>
                </div>
              )}
              {email.track_clicks && email.first_clicked_at && (
                <div className="space-y-1">
                  <p className="text-xs text-muted-foreground">First clicked</p>
                  <p title={fmtDateTime(email.first_clicked_at)}>
                    {fmtRelativeTime(email.first_clicked_at)}
                  </p>
                </div>
              )}
            </div>

            {trackingLinks && trackingLinks.length > 0 && (
              <div className="border-t pt-4">
                <p className="text-sm font-medium mb-2">Link clicks</p>
                <div className="space-y-2">
                  {trackingLinks.map((link) => (
                    <div
                      key={link.link_index}
                      className="flex items-center justify-between text-sm gap-4"
                    >
                      <a
                        href={link.original_url}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="text-blue-500 hover:underline truncate min-w-0 flex-1"
                        title={link.original_url}
                      >
                        {link.original_url}
                      </a>
                      <Badge
                        variant={link.click_count > 0 ? 'default' : 'secondary'}
                        className="shrink-0"
                      >
                        {link.click_count}{' '}
                        {link.click_count === 1 ? 'click' : 'clicks'}
                      </Badge>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </CardContent>
        </Card>
      ) : null}

      {defaultTab ? (
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-base">Email content</CardTitle>
          </CardHeader>
          <CardContent>
            <Tabs
              value={activeTab}
              onValueChange={(v) => patch({ tab: v })}
              className="w-full"
            >
              <TabsList className="grid w-full grid-cols-2 sm:grid-cols-3 mb-4 h-auto">
                {hasHtml && (
                  <TabsTrigger value="preview" className="gap-2">
                    <Eye className="h-4 w-4" />
                    Preview
                  </TabsTrigger>
                )}
                {hasHtml && (
                  <TabsTrigger value="html-source" className="gap-2">
                    <Code className="h-4 w-4" />
                    HTML Source
                  </TabsTrigger>
                )}
                {hasText && (
                  <TabsTrigger value="text" className="gap-2">
                    <FileText className="h-4 w-4" />
                    Plain Text
                  </TabsTrigger>
                )}
              </TabsList>

              {hasHtml && (
                <TabsContent value="preview" className="mt-0">
                  <HtmlPreview html={email.html_body!} />
                </TabsContent>
              )}

              {hasHtml && (
                <TabsContent value="html-source" className="mt-0">
                  <SourceView content={email.html_body!} type="html" />
                </TabsContent>
              )}

              {hasText && (
                <TabsContent value="text" className="mt-0">
                  <TextPreview text={email.text_body!} />
                </TabsContent>
              )}
            </Tabs>
          </CardContent>
        </Card>
      ) : null}

      {messageDetails.length > 0 ? (
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-base">Message details</CardTitle>
          </CardHeader>
          <CardContent>
            <KeyValueList items={messageDetails} />
          </CardContent>
        </Card>
      ) : null}

      {(email.track_opens || email.track_clicks) && (
        <EmailEventTimeline emailId={email.id} />
      )}
    </>
  )
}

function EmailDetailSkeleton({ backAction }: { backAction: ReactNode }) {
  return (
    <Detail
      title={<Skeleton className="h-7 w-64" />}
      description={<Skeleton className="mt-1 h-4 w-40" />}
      actions={backAction}
      facts={[0, 1, 2, 3].map(() => ({
        label: <Skeleton className="h-3 w-12" />,
        value: <Skeleton className="h-4 w-20" />,
      }))}
      main={
        <>
          <Skeleton className="h-56 w-full rounded-lg" />
          <Skeleton className="h-40 w-full rounded-lg" />
        </>
      }
      aside={<Skeleton className="h-48 w-full rounded-lg" />}
    />
  )
}

export function EmailDetail() {
  const { id } = useParams<{ id: string }>()
  const { setBreadcrumbs } = useBreadcrumbs()

  const {
    data: email,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...getEmailOptions({
      path: { id: id! },
    }),
    enabled: !!id,
  })

  usePageTitle(email ? `Email: ${email.subject}` : 'Email Details')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Email', href: '/email?tab=emails' },
      { label: email?.subject || 'Email Details' },
    ])
  }, [setBreadcrumbs, email?.subject])

  const backAction = (
    <Button variant="ghost" size="sm" asChild>
      <Link to="/email?tab=emails">
        <ArrowLeft className="h-4 w-4 mr-2" />
        Back to Emails
      </Link>
    </Button>
  )

  if (isLoading) {
    return <EmailDetailSkeleton backAction={backAction} />
  }

  if (error || !email) {
    return (
      <PageContainer>
        <div className="flex items-center gap-4">{backAction}</div>
        <PageState
          variant="failed"
          icon={AlertCircle}
          title="Couldn't load email"
          description="This email may not exist, or you may not have permission to view it."
          action={<Button onClick={() => void refetch()}>Retry</Button>}
        />
      </PageContainer>
    )
  }

  const verdict = emailVerdict(email)
  const headerItems: { key: string; value: ReactNode }[] = email.headers
    ? Object.entries(email.headers).map(([key, value]) => ({ key, value }))
    : []

  return (
    <Detail
      title={email.subject}
      description={
        <span className="inline-flex flex-wrap items-center gap-1.5">
          <code className="font-mono text-xs">{email.id}</code>
          <CopyAction value={email.id} label="Copy email ID" />
          {email.project_id != null && <span>· project #{email.project_id}</span>}
          {email.domain_id != null && <span>· domain #{email.domain_id}</span>}
        </span>
      }
      verdict={<Status tone={verdict.tone} label={verdict.label} />}
      actions={backAction}
      facts={emailFacts(email)}
      main={<EmailDetailContent email={email} />}
      aside={
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-base">Headers</CardTitle>
          </CardHeader>
          <CardContent>
            <KeyValueList items={headerItems} />
          </CardContent>
        </Card>
      }
    />
  )
}

export default EmailDetail
