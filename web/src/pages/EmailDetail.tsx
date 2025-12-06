'use client'

import { getEmailOptions } from '@/api/client/@tanstack/react-query.gen'
import { EmailResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertCircle,
  Archive,
  ArrowLeft,
  CheckCircle2,
  Clock,
  Code,
  Eye,
  FileText,
  Mail,
  Tag,
} from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { Link, useParams } from 'react-router-dom'

function StatusBadge({ status }: { status: string }) {
  switch (status) {
    case 'sent':
      return (
        <Badge variant="default" className="gap-1">
          <CheckCircle2 className="h-3 w-3" />
          Sent
        </Badge>
      )
    case 'queued':
      return (
        <Badge variant="secondary" className="gap-1">
          <Clock className="h-3 w-3" />
          Queued
        </Badge>
      )
    case 'failed':
      return (
        <Badge variant="destructive" className="gap-1">
          <AlertCircle className="h-3 w-3" />
          Failed
        </Badge>
      )
    case 'captured':
      return (
        <Badge variant="outline" className="gap-1 border-blue-500 text-blue-600">
          <Archive className="h-3 w-3" />
          Captured
        </Badge>
      )
    default:
      return <Badge variant="outline">{status}</Badge>
  }
}

function HeadersDisplay({ headers }: { headers: Record<string, string> | null | undefined }) {
  if (!headers) return null

  const entries = Object.entries(headers)
  if (entries.length === 0) {
    return <p className="text-sm text-muted-foreground">No headers available</p>
  }

  return (
    <div className="space-y-2">
      {entries.map(([key, value]) => (
        <div key={key} className="flex items-start gap-2 text-sm">
          <span className="font-medium min-w-[140px] text-muted-foreground">{key}:</span>
          <span className="font-mono text-xs break-all">{value}</span>
        </div>
      ))}
    </div>
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
            const newHeight = Math.max(300, Math.min(doc.body.scrollHeight + 40, 800))
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
      <pre className="text-sm whitespace-pre-wrap font-mono break-all">{text}</pre>
    </div>
  )
}

function SourceView({ content, type }: { content: string; type: 'html' | 'text' }) {
  return (
    <div className="relative">
      <div className="absolute top-2 right-2 z-10">
        <CopyButton
          value={content}
          className="h-8 w-8 p-0 hover:bg-accent hover:text-accent-foreground rounded-md bg-background/80 backdrop-blur-sm"
        />
      </div>
      <div className="border rounded-lg bg-muted/30 p-4 max-h-[600px] overflow-auto">
        <pre className="text-xs font-mono whitespace-pre-wrap break-all">
          {type === 'html' ? content : content}
        </pre>
      </div>
    </div>
  )
}

function EmailDetailContent({ email }: { email: EmailResponse }) {
  const hasHtml = !!email.html_body
  const hasText = !!email.text_body
  const defaultTab = hasHtml ? 'preview' : hasText ? 'text' : 'details'

  return (
    <div className="space-y-6">
      {/* Email Metadata */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Mail className="h-5 w-5" />
            Email Information
          </CardTitle>
        </CardHeader>
        <CardContent>
          <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
            <div className="space-y-4">
              <div>
                <h4 className="text-sm font-medium text-muted-foreground mb-1">From</h4>
                <p className="text-sm font-medium">
                  {email.from_name ? `${email.from_name} <${email.from_address}>` : email.from_address}
                </p>
              </div>

              <div>
                <h4 className="text-sm font-medium text-muted-foreground mb-1">To</h4>
                <div className="flex flex-wrap gap-1">
                  {email.to_addresses.map((addr) => (
                    <Badge key={addr} variant="secondary" className="font-mono text-xs">
                      {addr}
                    </Badge>
                  ))}
                </div>
              </div>

              {email.cc_addresses && email.cc_addresses.length > 0 && (
                <div>
                  <h4 className="text-sm font-medium text-muted-foreground mb-1">CC</h4>
                  <div className="flex flex-wrap gap-1">
                    {email.cc_addresses.map((addr) => (
                      <Badge key={addr} variant="outline" className="font-mono text-xs">
                        {addr}
                      </Badge>
                    ))}
                  </div>
                </div>
              )}

              {email.bcc_addresses && email.bcc_addresses.length > 0 && (
                <div>
                  <h4 className="text-sm font-medium text-muted-foreground mb-1">BCC</h4>
                  <div className="flex flex-wrap gap-1">
                    {email.bcc_addresses.map((addr) => (
                      <Badge key={addr} variant="outline" className="font-mono text-xs">
                        {addr}
                      </Badge>
                    ))}
                  </div>
                </div>
              )}

              {email.reply_to && (
                <div>
                  <h4 className="text-sm font-medium text-muted-foreground mb-1">Reply-To</h4>
                  <p className="text-sm font-mono">{email.reply_to}</p>
                </div>
              )}
            </div>

            <div className="space-y-4">
              <div>
                <h4 className="text-sm font-medium text-muted-foreground mb-1">Status</h4>
                <StatusBadge status={email.status} />
              </div>

              <div>
                <h4 className="text-sm font-medium text-muted-foreground mb-1">
                  {email.sent_at ? 'Sent At' : 'Created At'}
                </h4>
                <p className="text-sm">
                  {format(new Date(email.sent_at || email.created_at), 'PPpp')}
                </p>
              </div>

              {email.provider_message_id && (
                <div>
                  <h4 className="text-sm font-medium text-muted-foreground mb-1">
                    Provider Message ID
                  </h4>
                  <div className="flex items-center gap-2">
                    <code className="text-xs font-mono bg-muted px-2 py-1 rounded break-all">
                      {email.provider_message_id}
                    </code>
                    <CopyButton
                      value={email.provider_message_id}
                      className="h-6 w-6 p-0 hover:bg-accent hover:text-accent-foreground rounded-md shrink-0"
                    />
                  </div>
                </div>
              )}

              {email.tags && email.tags.length > 0 && (
                <div>
                  <h4 className="text-sm font-medium text-muted-foreground mb-1 flex items-center gap-1">
                    <Tag className="h-3 w-3" />
                    Tags
                  </h4>
                  <div className="flex flex-wrap gap-1">
                    {email.tags.map((tag) => (
                      <Badge key={tag} variant="outline">
                        {tag}
                      </Badge>
                    ))}
                  </div>
                </div>
              )}
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Subject */}
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">Subject</CardTitle>
        </CardHeader>
        <CardContent>
          <p className="text-lg font-medium">{email.subject}</p>
        </CardContent>
      </Card>

      {/* Error Message */}
      {email.error_message && (
        <Card className="border-destructive">
          <CardHeader className="pb-3">
            <CardTitle className="text-base text-destructive flex items-center gap-2">
              <AlertCircle className="h-4 w-4" />
              Error
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-sm text-destructive">{email.error_message}</p>
          </CardContent>
        </Card>
      )}

      {/* Email Content */}
      {(hasHtml || hasText) && (
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-base">Email Content</CardTitle>
          </CardHeader>
          <CardContent>
            <Tabs defaultValue={defaultTab} className="w-full">
              <TabsList className="grid w-full grid-cols-4 mb-4">
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
                <TabsTrigger value="headers" className="gap-2">
                  Headers
                </TabsTrigger>
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

              <TabsContent value="headers" className="mt-0">
                <div className="border rounded-lg p-4 bg-muted/30">
                  <HeadersDisplay headers={email.headers} />
                </div>
              </TabsContent>
            </Tabs>
          </CardContent>
        </Card>
      )}

      {/* Technical Details */}
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">Technical Details</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="grid grid-cols-1 md:grid-cols-3 gap-4 text-sm">
            <div>
              <span className="text-muted-foreground">Email ID:</span>
              <div className="flex items-center gap-2 mt-1">
                <code className="font-mono text-xs bg-muted px-2 py-1 rounded break-all">
                  {email.id}
                </code>
                <CopyButton
                  value={email.id}
                  className="h-6 w-6 p-0 hover:bg-accent hover:text-accent-foreground rounded-md shrink-0"
                />
              </div>
            </div>
            {email.domain_id && (
              <div>
                <span className="text-muted-foreground">Domain ID:</span>
                <p className="font-mono text-xs mt-1">{email.domain_id}</p>
              </div>
            )}
            {email.project_id && (
              <div>
                <span className="text-muted-foreground">Project ID:</span>
                <p className="font-mono text-xs mt-1">{email.project_id}</p>
              </div>
            )}
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

function LoadingSkeleton() {
  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <Skeleton className="h-6 w-40" />
        </CardHeader>
        <CardContent>
          <div className="grid grid-cols-2 gap-6">
            <div className="space-y-4">
              <Skeleton className="h-4 w-full" />
              <Skeleton className="h-4 w-3/4" />
              <Skeleton className="h-4 w-1/2" />
            </div>
            <div className="space-y-4">
              <Skeleton className="h-4 w-full" />
              <Skeleton className="h-4 w-3/4" />
            </div>
          </div>
        </CardContent>
      </Card>
      <Card>
        <CardHeader>
          <Skeleton className="h-6 w-24" />
        </CardHeader>
        <CardContent>
          <Skeleton className="h-8 w-full" />
        </CardContent>
      </Card>
      <Card>
        <CardHeader>
          <Skeleton className="h-6 w-32" />
        </CardHeader>
        <CardContent>
          <Skeleton className="h-[300px] w-full" />
        </CardContent>
      </Card>
    </div>
  )
}

export function EmailDetail() {
  const { id } = useParams<{ id: string }>()
  const { setBreadcrumbs } = useBreadcrumbs()

  const {
    data: email,
    isLoading,
    error,
  } = useQuery({
    ...getEmailOptions({
      path: { id: id! },
    }),
    enabled: !!id,
  })

  usePageTitle(email ? `Email: ${email.subject}` : 'Email Details')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Email', href: '/email' },
      { label: email?.subject || 'Email Details' },
    ])
  }, [setBreadcrumbs, email?.subject])

  if (isLoading) {
    return (
      <div className="container max-w-6xl mx-auto py-6 space-y-6">
        <div className="flex items-center gap-4">
          <Button variant="ghost" size="sm" asChild>
            <Link to="/email">
              <ArrowLeft className="h-4 w-4 mr-2" />
              Back to Emails
            </Link>
          </Button>
        </div>
        <LoadingSkeleton />
      </div>
    )
  }

  if (error || !email) {
    return (
      <div className="container max-w-6xl mx-auto py-6 space-y-6">
        <div className="flex items-center gap-4">
          <Button variant="ghost" size="sm" asChild>
            <Link to="/email">
              <ArrowLeft className="h-4 w-4 mr-2" />
              Back to Emails
            </Link>
          </Button>
        </div>
        <Card className="border-destructive">
          <CardContent className="pt-6">
            <div className="flex items-center gap-2 text-destructive">
              <AlertCircle className="h-5 w-5" />
              <p>Failed to load email details. The email may not exist or you may not have permission to view it.</p>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="container max-w-6xl mx-auto py-6 space-y-6">
      <div className="flex items-center gap-4">
        <Button variant="ghost" size="sm" asChild>
          <Link to="/email">
            <ArrowLeft className="h-4 w-4 mr-2" />
            Back to Emails
          </Link>
        </Button>
      </div>

      <EmailDetailContent email={email} />
    </div>
  )
}

export default EmailDetail
