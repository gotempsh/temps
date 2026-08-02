import { ProjectResponse } from '@/api/client'
import { blobStatusOptions } from '@/api/client/@tanstack/react-query.gen'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useEffect } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  HardDrive,
  CheckCircle2,
  XCircle,
  Info,
  Terminal,
  BookOpen,
  Settings2,
  ExternalLink,
  Upload,
  Search,
  Layers,
  Copy,
} from 'lucide-react'
import { Skeleton } from '@/components/ui/skeleton'
import { CopyButton } from '@/components/ui/copy-button'
import { Link } from 'react-router'

interface BlobServiceProps {
  project: ProjectResponse
}

export function BlobService({ project: _project }: BlobServiceProps) {
  const { setBreadcrumbs } = useBreadcrumbs()

  const { data: status, isLoading } = useQuery({
    ...blobStatusOptions(),
    refetchInterval: 10000,
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Databases', href: `../storage` },
      { label: 'Blob Storage' },
    ])
  }, [setBreadcrumbs])

  const isEnabled = status?.enabled ?? false

  if (isLoading) {
    return (
      <div className="space-y-6">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="p-2 rounded-lg bg-primary/10">
              <HardDrive className="h-6 w-6 text-primary" />
            </div>
            <div>
              <h1 className="text-xl font-semibold sm:text-2xl">Blob Storage</h1>
              <p className="text-muted-foreground text-sm">
                S3-compatible file storage
              </p>
            </div>
          </div>
          <Skeleton className="h-7 w-24" />
        </div>
        <Card>
          <CardHeader>
            <Skeleton className="h-6 w-32" />
            <Skeleton className="h-4 w-48" />
          </CardHeader>
          <CardContent className="space-y-4">
            <Skeleton className="h-20 w-full" />
            <Skeleton className="h-10 w-32" />
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex items-center gap-3 min-w-0">
          <div className="p-2 rounded-lg bg-primary/10 shrink-0">
            <HardDrive className="h-6 w-6 text-primary" />
          </div>
          <div className="min-w-0">
            <h1 className="text-xl font-semibold sm:text-2xl">Blob Storage</h1>
            <p className="text-muted-foreground text-sm">
              Upload, list, and serve files with a simple S3-backed API
            </p>
          </div>
        </div>
        <Badge
          variant={isEnabled ? 'default' : 'secondary'}
          className="h-7 px-3 self-start sm:self-auto shrink-0"
        >
          {isEnabled ? (
            <>
              <CheckCircle2 className="h-3.5 w-3.5 mr-1.5" />
              Enabled
            </>
          ) : (
            <>
              <XCircle className="h-3.5 w-3.5 mr-1.5" />
              Disabled
            </>
          )}
        </Badge>
      </div>

      <Tabs defaultValue="overview" className="space-y-6">
        <TabsList>
          <TabsTrigger value="overview" className="gap-2">
            <Settings2 className="h-4 w-4" />
            Overview
          </TabsTrigger>
          <TabsTrigger value="docs" className="gap-2">
            <BookOpen className="h-4 w-4" />
            Documentation
          </TabsTrigger>
          <TabsTrigger value="examples" className="gap-2">
            <Terminal className="h-4 w-4" />
            Examples
          </TabsTrigger>
        </TabsList>

        <TabsContent value="overview" className="space-y-6">
          <Card>
            <CardHeader>
              <CardTitle>Service Status</CardTitle>
              <CardDescription>
                Cluster-wide status of Blob Storage. Once enabled, every project on this instance can store files through the SDK below.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {isEnabled ? (
                <div className="space-y-4">
                  <div className="grid gap-4 sm:grid-cols-3">
                    <div className="p-4 rounded-lg border bg-muted/30">
                      <p className="text-sm text-muted-foreground">Status</p>
                      <p className={`font-medium flex items-center gap-1.5 mt-1 ${status?.healthy ? 'text-green-600' : 'text-red-600'}`}>
                        {status?.healthy ? (
                          <CheckCircle2 className="h-4 w-4" />
                        ) : (
                          <XCircle className="h-4 w-4" />
                        )}
                        {status?.healthy ? 'Healthy' : 'Unhealthy'}
                      </p>
                    </div>
                    <div className="p-4 rounded-lg border bg-muted/30">
                      <p className="text-sm text-muted-foreground">Engine</p>
                      <p className="font-medium mt-1">{status?.version || 'S3-compatible'}</p>
                    </div>
                    <div className="p-4 rounded-lg border bg-muted/30">
                      <p className="text-sm text-muted-foreground">Docker Image</p>
                      <p className="font-medium mt-1 font-mono text-xs break-all">
                        {status?.docker_image || 'Unknown'}
                      </p>
                    </div>
                  </div>
                  <Button variant="outline" asChild>
                    <Link to="/storage?tab=platform" className="gap-2">
                      <ExternalLink className="h-4 w-4" />
                      Manage in Storage Settings
                    </Link>
                  </Button>
                </div>
              ) : (
                <div className="space-y-4">
                  <Alert>
                    <Info className="h-4 w-4" />
                    <AlertTitle>Blob Storage is not enabled</AlertTitle>
                    <AlertDescription>
                      An administrator must enable the Blob service from <strong>Storage Settings → Platform Services</strong>. Once enabled, the SDK on the Documentation tab works out of the box — no further per-project setup needed.
                    </AlertDescription>
                  </Alert>
                  <Button asChild>
                    <Link to="/storage?tab=platform" className="gap-2">
                      <ExternalLink className="h-4 w-4" />
                      Enable in Storage Settings
                    </Link>
                  </Button>
                </div>
              )}
            </CardContent>
          </Card>

          <div className="grid gap-4 md:grid-cols-2">
            <FeatureCard
              icon={Upload}
              title="Streaming uploads"
              description="Accept any body type — strings, Buffers, Uint8Arrays, Blobs, or ReadableStreams — without buffering large files into memory."
            />
            <FeatureCard
              icon={Search}
              title="Prefix listing"
              description="List files under a path prefix with cursor-based pagination. Build folder-style browsing on top of a flat namespace."
            />
            <FeatureCard
              icon={Layers}
              title="Auto MIME detection"
              description="Content type is inferred from the file extension. Override it explicitly when serving compressed or non-standard formats."
            />
            <FeatureCard
              icon={Copy}
              title="Server-side copy"
              description="Duplicate a blob to a new path without re-uploading the bytes. Ideal for backups, snapshots, and asset versioning."
            />
          </div>
        </TabsContent>

        <TabsContent value="docs" className="space-y-6">
          <Card>
            <CardHeader>
              <CardTitle>TypeScript SDK</CardTitle>
              <CardDescription>
                The <code className="bg-muted px-1.5 py-0.5 rounded text-xs">@temps-sdk/blob</code>{' '}
                package provides a typed client for upload, download, list, copy, and delete from any
                Node.js or Bun runtime.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              <div className="space-y-3">
                <h3 className="font-medium">Installation</h3>
                <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
                  <CodeBlock code="npm install @temps-sdk/blob" />
                  <CodeBlock code="bun add @temps-sdk/blob" />
                  <CodeBlock code="pnpm add @temps-sdk/blob" />
                  <CodeBlock code="yarn add @temps-sdk/blob" />
                </div>
              </div>

              <div className="space-y-3">
                <h3 className="font-medium">Quick start</h3>
                <p className="text-sm text-muted-foreground">
                  The default <code className="bg-muted px-1.5 py-0.5 rounded text-xs">blob</code>{' '}
                  singleton reads its config from environment variables — no extra wiring required
                  when running on Temps.
                </p>
                <CodeBlock
                  code={`import { blob } from '@temps-sdk/blob'

// Upload
const { url } = await blob.put('avatars/user-123.png', fileBuffer)

// Download
const response = await blob.download(url)
const data = await response.arrayBuffer()

// List
const { blobs } = await blob.list({ prefix: 'avatars/' })

// Delete
await blob.del(url)`}
                  language="typescript"
                />
              </div>

              <div className="space-y-3">
                <h3 className="font-medium">Configuration</h3>
                <p className="text-sm text-muted-foreground">
                  These environment variables are injected automatically into deployments running on
                  this instance. Set them yourself only when running locally or outside of Temps.
                </p>
                <CodeBlock
                  code={`# Required
TEMPS_API_URL=https://your-instance.temps.dev   # API endpoint
TEMPS_TOKEN=your-token                          # API key or deployment token

# Required for API keys (deployment tokens embed the project ID)
TEMPS_PROJECT_ID=42`}
                  language="bash"
                />
                <p className="text-sm text-muted-foreground">
                  Need an isolated client (multiple projects, custom timeouts, testing)? Use{' '}
                  <code className="bg-muted px-1.5 py-0.5 rounded text-xs">createClient</code>:
                </p>
                <CodeBlock
                  code={`import { createClient } from '@temps-sdk/blob'

const storage = createClient({
  apiUrl: 'https://your-instance.temps.dev',
  token: process.env.TEMPS_TOKEN,
  projectId: 42, // optional with deployment tokens
})`}
                  language="typescript"
                />
              </div>

              <div className="space-y-4">
                <h3 className="font-medium">API Reference</h3>

                <ApiMethod
                  name="put"
                  description="Upload a file. Content type is auto-detected from the extension; override explicitly when needed. Accepts string | ArrayBuffer | Uint8Array | Blob | ReadableStream<Uint8Array> | Buffer."
                  signature={`put(
  pathname: string,
  body: BlobBody,
  options?: {
    contentType?: string         // override auto-detection
    addRandomSuffix?: boolean    // default: true — prevents collisions
    cacheControl?: string        // Cache-Control header
    contentEncoding?: string     // e.g., 'gzip'
    contentDisposition?: string  // e.g., 'attachment; filename="x.txt"'
  },
): Promise<BlobInfo>`}
                  example={`const result = await blob.put('images/avatar.png', imageBuffer, {
  contentType: 'image/png',
  cacheControl: 'public, max-age=31536000, immutable',
})

console.log(result.url)         // https://your-instance.temps.dev/api/blob/...
console.log(result.pathname)    // images/avatar-abc123.png
console.log(result.contentType) // image/png
console.log(result.size)        // 12345
console.log(result.uploadedAt)  // 2026-01-15T10:30:00.000Z`}
                />

                <ApiMethod
                  name="del"
                  description="Delete one or more files by URL or pathname. Accepts a single value or an array."
                  signature="del(urls: string | string[]): Promise<void>"
                  example={`// Delete one
await blob.del(fileUrl)

// Delete many
await blob.del([urlA, urlB, urlC])`}
                />

                <ApiMethod
                  name="head"
                  description="Get metadata for a file without downloading its contents."
                  signature="head(url: string): Promise<BlobInfo>"
                  example={`const info = await blob.head(fileUrl)

console.log(info.contentType) // 'application/pdf'
console.log(info.size)        // 1234567
console.log(info.uploadedAt)  // ISO 8601 timestamp`}
                />

                <ApiMethod
                  name="list"
                  description="List files with optional prefix filtering and cursor-based pagination."
                  signature={`list(options?: {
  limit?: number    // default: 1000
  prefix?: string   // filter by path prefix
  cursor?: string   // pagination cursor from previous response
}): Promise<{
  blobs: BlobInfo[]
  hasMore: boolean
  cursor?: string
}>`}
                  example={`// All files
const { blobs, hasMore, cursor } = await blob.list()

// Under a prefix
const images = await blob.list({ prefix: 'images/', limit: 50 })

// Paginate fully
let page = await blob.list({ limit: 100 })
while (page.hasMore) {
  for (const file of page.blobs) {
    console.log(file.pathname, file.size)
  }
  page = await blob.list({ limit: 100, cursor: page.cursor })
}`}
                />

                <ApiMethod
                  name="download"
                  description="Download a file. Returns a standard Web Response — read it as text, arrayBuffer, or stream it directly."
                  signature="download(url: string): Promise<Response>"
                  example={`const response = await blob.download(fileUrl)

// As text
const text = await response.text()

// As binary
const buffer = await response.arrayBuffer()

// Stream to disk in Node.js
import { writeFile } from 'node:fs/promises'
await writeFile('./downloaded.png', Buffer.from(await response.arrayBuffer()))`}
                />

                <ApiMethod
                  name="copy"
                  description="Duplicate a blob to a new pathname server-side — no re-upload, no client bandwidth."
                  signature="copy(fromUrl: string, toPathname: string): Promise<BlobInfo>"
                  example={`const snapshot = await blob.copy(
  liveAssetUrl,
  \`backups/\${new Date().toISOString()}/asset.png\`,
)
console.log(snapshot.url)`}
                />
              </div>

              <div className="space-y-3">
                <h3 className="font-medium">Types</h3>
                <CodeBlock
                  code={`interface BlobInfo {
  url: string         // Full URL to access the blob
  pathname: string    // Path within project storage
  contentType: string // MIME type (e.g., 'image/png')
  size: number        // Size in bytes
  uploadedAt: string  // ISO 8601 timestamp
}

type BlobBody =
  | string
  | ArrayBuffer
  | Uint8Array
  | Blob
  | ReadableStream<Uint8Array>
  | Buffer`}
                  language="typescript"
                />
              </div>

              <div className="space-y-3">
                <h3 className="font-medium">Error handling</h3>
                <p className="text-sm text-muted-foreground">
                  Every error thrown by the SDK is an instance of{' '}
                  <code className="bg-muted px-1.5 py-0.5 rounded text-xs">BlobError</code> with
                  structured fields.
                </p>
                <CodeBlock
                  code={`import { blob, BlobError } from '@temps-sdk/blob'

try {
  await blob.head('nonexistent.txt')
} catch (error) {
  if (error instanceof BlobError) {
    console.error(error.message) // 'Blob not found: nonexistent.txt'
    console.error(error.code)    // 'NOT_FOUND' | 'MISSING_CONFIG' | 'NETWORK_ERROR' | 'INVALID_INPUT'
    console.error(error.status)  // HTTP status (when applicable)
    console.error(error.title)   // RFC 7807 problem title
    console.error(error.detail)  // RFC 7807 problem detail
  }
}`}
                  language="typescript"
                />
              </div>
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="examples" className="space-y-6">
          <Card>
            <CardHeader>
              <CardTitle>Usage Patterns</CardTitle>
              <CardDescription>
                Battle-tested recipes for common file storage workflows.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              <ExampleSection
                title="Avatar upload (Next.js Route Handler)"
                description="Accept a multipart upload, validate type, then store with long-lived caching."
                code={`import { blob } from '@temps-sdk/blob'

export async function POST(request: Request) {
  const form = await request.formData()
  const file = form.get('avatar') as File

  if (!file) {
    return Response.json({ error: 'No file provided' }, { status: 400 })
  }

  const allowed = ['image/jpeg', 'image/png', 'image/webp']
  if (!allowed.includes(file.type)) {
    return Response.json({ error: 'Invalid file type' }, { status: 400 })
  }

  const { url } = await blob.put(
    \`avatars/\${userId}/\${file.name}\`,
    await file.arrayBuffer(),
    {
      contentType: file.type,
      addRandomSuffix: false,
      cacheControl: 'public, max-age=31536000, immutable',
    },
  )

  return Response.json({ url })
}`}
              />

              <ExampleSection
                title="Backup and restore"
                description="Snapshot JSON state on a schedule, list recent backups, and restore the latest."
                code={`import { blob } from '@temps-sdk/blob'

// Snapshot
const data = JSON.stringify(await db.export())
await blob.put(
  \`backups/\${new Date().toISOString()}.json\`,
  data,
  { contentType: 'application/json' },
)

// List the 10 most recent
const { blobs } = await blob.list({ prefix: 'backups/', limit: 10 })

// Restore the most recent
const latest = blobs[blobs.length - 1]
const response = await blob.download(latest.url)
const backup = await response.json()`}
              />

              <ExampleSection
                title="Content-addressed asset pipeline"
                description="Hash the file contents to derive an immutable URL — ideal for static asset deploys with infinite caching."
                code={`import { readFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { blob } from '@temps-sdk/blob'

async function uploadAsset(filePath: string) {
  const content = readFileSync(filePath)
  const hash = createHash('md5').update(content).digest('hex').slice(0, 8)
  const ext = filePath.split('.').pop()

  const { url } = await blob.put(\`assets/\${hash}.\${ext}\`, content, {
    addRandomSuffix: false,
    cacheControl: 'public, max-age=31536000, immutable',
  })

  return url
}`}
              />

              <ExampleSection
                title="Cleanup stale uploads"
                description="Sweep a prefix on a schedule and delete files older than a cutoff."
                code={`import { blob } from '@temps-sdk/blob'

async function cleanupOlderThan(prefix: string, maxAgeMs: number) {
  const { blobs } = await blob.list({ prefix })
  const cutoff = Date.now() - maxAgeMs

  const stale = blobs.filter(
    (b) => new Date(b.uploadedAt).getTime() < cutoff,
  )

  if (stale.length > 0) {
    await blob.del(stale.map((b) => b.url))
    console.log(\`Deleted \${stale.length} stale files\`)
  }
}

// Drop temp uploads older than a week
await cleanupOlderThan('temp/', 7 * 24 * 60 * 60 * 1000)`}
              />

              <ExampleSection
                title="Streaming download with caching headers"
                description="Proxy blob downloads through a Route Handler so you can attach Cache-Control or Content-Disposition headers."
                code={`import { blob } from '@temps-sdk/blob'

export async function GET(
  _request: Request,
  { params }: { params: { url: string } },
) {
  try {
    const url = decodeURIComponent(params.url)
    const info = await blob.head(url)
    const response = await blob.download(url)

    return new Response(response.body, {
      headers: {
        'Content-Type': info.contentType,
        'Content-Length': info.size.toString(),
        'Content-Disposition': \`attachment; filename="\${info.pathname.split('/').pop()}"\`,
        'Cache-Control': 'public, max-age=31536000, immutable',
      },
    })
  } catch {
    return Response.json({ error: 'File not found' }, { status: 404 })
  }
}`}
              />
            </CardContent>
          </Card>
        </TabsContent>
      </Tabs>
    </div>
  )
}

function FeatureCard({
  icon: Icon,
  title,
  description,
}: {
  icon: typeof Upload
  title: string
  description: string
}) {
  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-center gap-2">
          <div className="p-1.5 rounded-md bg-primary/10">
            <Icon className="h-4 w-4 text-primary" />
          </div>
          <CardTitle className="text-base">{title}</CardTitle>
        </div>
      </CardHeader>
      <CardContent>
        <p className="text-sm text-muted-foreground">{description}</p>
      </CardContent>
    </Card>
  )
}

function CodeBlock({ code, language: _language = 'bash' }: { code: string; language?: string }) {
  return (
    <div className="relative">
      <pre className="bg-muted rounded-lg p-3 text-sm font-mono overflow-x-auto">
        <code>{code}</code>
      </pre>
      <CopyButton
        value={code}
        className="absolute top-1.5 right-1.5 h-7 w-7 p-0 hover:bg-accent hover:text-accent-foreground rounded-md"
      />
    </div>
  )
}

function ApiMethod({
  name,
  description,
  signature,
  example,
}: {
  name: string
  description: string
  signature: string
  example: string
}) {
  return (
    <div className="border rounded-lg p-4 space-y-3">
      <div>
        <h4 className="font-medium font-mono text-primary">{name}</h4>
        <p className="text-sm text-muted-foreground whitespace-pre-line mt-1">
          {description}
        </p>
      </div>
      <div>
        <p className="text-xs text-muted-foreground mb-1">Signature</p>
        <pre className="bg-muted rounded px-2 py-1 text-xs font-mono overflow-x-auto whitespace-pre-wrap">
          {signature}
        </pre>
      </div>
      <CodeBlock code={example} language="typescript" />
    </div>
  )
}

function ExampleSection({
  title,
  description,
  code,
}: {
  title: string
  description: string
  code: string
}) {
  return (
    <div className="space-y-3">
      <div>
        <h3 className="font-medium">{title}</h3>
        <p className="text-sm text-muted-foreground">{description}</p>
      </div>
      <CodeBlock code={code} language="typescript" />
    </div>
  )
}
