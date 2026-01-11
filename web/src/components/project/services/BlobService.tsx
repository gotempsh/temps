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
} from 'lucide-react'
import { Skeleton } from '@/components/ui/skeleton'
import { CopyButton } from '@/components/ui/copy-button'
import { Link } from 'react-router-dom'

interface BlobServiceProps {
  project: ProjectResponse
}

export function BlobService({ project: _project }: BlobServiceProps) {
  const { setBreadcrumbs } = useBreadcrumbs()

  // Fetch Blob status
  const { data: status, isLoading } = useQuery({
    ...blobStatusOptions(),
    refetchInterval: 10000, // Refetch every 10 seconds
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Services', href: `../services` },
      { label: 'Blob Storage' },
    ])
  }, [setBreadcrumbs])

  const isEnabled = status?.enabled ?? false

  // Show loading skeleton while fetching status
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
                S3-compatible object storage
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
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <div className="p-2 rounded-lg bg-primary/10">
            <HardDrive className="h-6 w-6 text-primary" />
          </div>
          <div>
            <h1 className="text-xl font-semibold sm:text-2xl">Blob Storage</h1>
            <p className="text-muted-foreground text-sm">
              S3-compatible object storage
            </p>
          </div>
        </div>
        <Badge
          variant={isEnabled ? 'default' : 'secondary'}
          className="h-7 px-3"
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
                View your Blob Storage service status
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {isEnabled ? (
                <div className="space-y-4">
                  <div className="grid gap-4 sm:grid-cols-3">
                    <div className="p-4 rounded-lg border bg-muted/30">
                      <p className="text-sm text-muted-foreground">Status</p>
                      <p className="font-medium text-green-600 flex items-center gap-1.5 mt-1">
                        <CheckCircle2 className="h-4 w-4" />
                        {status?.healthy ? 'Healthy' : 'Unhealthy'}
                      </p>
                    </div>
                    <div className="p-4 rounded-lg border bg-muted/30">
                      <p className="text-sm text-muted-foreground">Version</p>
                      <p className="font-medium mt-1">{status?.version || 'Unknown'}</p>
                    </div>
                    <div className="p-4 rounded-lg border bg-muted/30">
                      <p className="text-sm text-muted-foreground">Docker Image</p>
                      <p className="font-medium mt-1 font-mono text-sm">
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
                      The Blob Storage service needs to be enabled by a system administrator.
                      Once enabled, you can use the SDK below to interact with it.
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
        </TabsContent>

        <TabsContent value="docs" className="space-y-6">
          <Card>
            <CardHeader>
              <CardTitle>TypeScript SDK</CardTitle>
              <CardDescription>
                Install and use the Temps Blob package in your TypeScript/JavaScript application
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              {/* Installation */}
              <div className="space-y-3">
                <h3 className="font-medium">Installation</h3>
                <div className="relative">
                  <pre className="bg-muted rounded-lg p-4 text-sm font-mono overflow-x-auto">
                    <code>npm install @temps-sdk/blob</code>
                  </pre>
                  <CopyButton
                    value="npm install @temps-sdk/blob"
                    className="absolute top-2 right-2 h-8 w-8 p-0 hover:bg-accent hover:text-accent-foreground rounded-md"
                  />
                </div>
                <p className="text-sm text-muted-foreground">
                  Or using other package managers:
                </p>
                <div className="grid gap-2 sm:grid-cols-3">
                  <CodeBlock code="yarn add @temps-sdk/blob" />
                  <CodeBlock code="pnpm add @temps-sdk/blob" />
                  <CodeBlock code="bun add @temps-sdk/blob" />
                </div>
              </div>

              {/* Configuration */}
              <div className="space-y-3">
                <h3 className="font-medium">Configuration</h3>
                <p className="text-sm text-muted-foreground">
                  The Blob client automatically reads the <code className="bg-muted px-1.5 py-0.5 rounded text-xs">TEMPS_BLOB_URL</code> environment
                  variable which is injected into your project's runtime.
                </p>
                <CodeBlock
                  code={`import { createBlobClient } from '@temps-sdk/blob'

// Automatically uses TEMPS_BLOB_URL from environment
const blob = createBlobClient()

// Or configure manually
const blob = createBlobClient({
  url: process.env.TEMPS_BLOB_URL,
  token: process.env.TEMPS_PROJECT_TOKEN,
})`}
                  language="typescript"
                />
              </div>

              {/* API Reference */}
              <div className="space-y-4">
                <h3 className="font-medium">API Reference</h3>

                <ApiMethod
                  name="put"
                  description="Upload a blob to storage"
                  signature={`blob.put(pathname: string, body: Blob | Buffer | string, options?: PutOptions): Promise<BlobInfo>`}
                  example={`const result = await blob.put('images/avatar.png', imageBuffer, {
  contentType: 'image/png',
  addRandomSuffix: true, // Prevents collisions
})

console.log(result.url) // /api/blob/123/images/avatar-abc123.png
console.log(result.pathname) // images/avatar-abc123.png
console.log(result.contentType) // image/png
console.log(result.size) // 12345`}
                />

                <ApiMethod
                  name="del"
                  description="Delete one or more blobs"
                  signature="blob.del(...pathnames: string[]): Promise<number>"
                  example={`// Delete single blob
await blob.del('images/avatar.png')

// Delete multiple blobs
const deleted = await blob.del(
  'images/old1.png',
  'images/old2.png',
  'documents/draft.pdf'
)
console.log(\`Deleted \${deleted} blobs\`)`}
                />

                <ApiMethod
                  name="head"
                  description="Get blob metadata without downloading content"
                  signature="blob.head(pathname: string): Promise<BlobInfo>"
                  example={`const info = await blob.head('documents/report.pdf')

console.log(info.contentType) // application/pdf
console.log(info.size) // 1234567
console.log(info.uploadedAt) // 2025-01-03T12:00:00Z`}
                />

                <ApiMethod
                  name="list"
                  description="List blobs with optional filtering and pagination"
                  signature={`blob.list(options?: ListOptions): Promise<ListResult>`}
                  example={`// List all blobs
const { blobs, hasMore, cursor } = await blob.list()

// List with prefix filter
const images = await blob.list({
  prefix: 'images/',
  limit: 100,
})

// Paginate through results
let cursor: string | undefined
do {
  const result = await blob.list({ cursor, limit: 50 })
  for (const item of result.blobs) {
    console.log(item.pathname, item.size)
  }
  cursor = result.cursor
} while (cursor)`}
                />

                <ApiMethod
                  name="download"
                  description="Download blob content as a readable stream"
                  signature="blob.download(pathname: string): Promise<ReadableStream>"
                  example={`// Download as stream
const stream = await blob.download('documents/report.pdf')

// In Node.js, pipe to file
import { createWriteStream } from 'fs'
import { Readable } from 'stream'

const nodeStream = Readable.fromWeb(stream)
nodeStream.pipe(createWriteStream('./report.pdf'))

// Or collect as buffer
const chunks: Uint8Array[] = []
for await (const chunk of stream) {
  chunks.push(chunk)
}
const buffer = Buffer.concat(chunks)`}
                />

                <ApiMethod
                  name="getUrl"
                  description="Get a public URL for a blob"
                  signature="blob.getUrl(pathname: string): string"
                  example={`const imageUrl = blob.getUrl('images/avatar.png')
// Returns: /api/blob/123/images/avatar.png

// Use in HTML
<img src={imageUrl} alt="Avatar" />`}
                />
              </div>

              {/* Types */}
              <div className="space-y-3">
                <h3 className="font-medium">Types</h3>
                <CodeBlock
                  code={`interface BlobInfo {
  url: string          // Full URL to access the blob
  pathname: string     // Path within project storage
  contentType: string  // MIME type (e.g., 'image/png')
  size: number         // Size in bytes
  uploadedAt: string   // ISO 8601 timestamp
}

interface PutOptions {
  contentType?: string     // Override auto-detected content type
  addRandomSuffix?: boolean // Add random suffix to prevent collisions
}

interface ListOptions {
  limit?: number     // Max items to return (default: 100)
  prefix?: string    // Filter by path prefix
  cursor?: string    // Pagination cursor
}

interface ListResult {
  blobs: BlobInfo[]       // List of blobs
  cursor?: string         // Cursor for next page
  hasMore: boolean        // Whether more results exist
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
              <CardTitle>Usage Examples</CardTitle>
              <CardDescription>
                Common patterns and use cases for Blob Storage
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              <ExampleSection
                title="File Upload API Route"
                description="Handle file uploads in Next.js or similar frameworks"
                code={`import { createBlobClient } from '@temps-sdk/blob'
import { NextRequest, NextResponse } from 'next/server'

const blob = createBlobClient()

export async function POST(request: NextRequest) {
  const formData = await request.formData()
  const file = formData.get('file') as File

  if (!file) {
    return NextResponse.json(
      { error: 'No file provided' },
      { status: 400 }
    )
  }

  // Validate file type
  const allowedTypes = ['image/jpeg', 'image/png', 'image/webp']
  if (!allowedTypes.includes(file.type)) {
    return NextResponse.json(
      { error: 'Invalid file type' },
      { status: 400 }
    )
  }

  // Upload to blob storage
  const buffer = Buffer.from(await file.arrayBuffer())
  const result = await blob.put(
    \`uploads/\${file.name}\`,
    buffer,
    {
      contentType: file.type,
      addRandomSuffix: true,
    }
  )

  return NextResponse.json({
    url: result.url,
    pathname: result.pathname,
    size: result.size,
  })
}`}
              />

              <ExampleSection
                title="Image Optimization Pipeline"
                description="Process and store optimized images"
                code={`import { createBlobClient } from '@temps-sdk/blob'
import sharp from 'sharp'

const blob = createBlobClient()

interface ImageVariant {
  width: number
  height: number
  suffix: string
}

const variants: ImageVariant[] = [
  { width: 1920, height: 1080, suffix: 'large' },
  { width: 800, height: 600, suffix: 'medium' },
  { width: 400, height: 300, suffix: 'small' },
  { width: 150, height: 150, suffix: 'thumb' },
]

async function processImage(
  originalBuffer: Buffer,
  filename: string
): Promise<Record<string, string>> {
  const results: Record<string, string> = {}
  const baseName = filename.replace(/\\.[^.]+$/, '')

  // Store original
  const original = await blob.put(
    \`images/\${filename}\`,
    originalBuffer,
    { addRandomSuffix: true }
  )
  results.original = original.url

  // Generate variants
  for (const variant of variants) {
    const resized = await sharp(originalBuffer)
      .resize(variant.width, variant.height, { fit: 'cover' })
      .webp({ quality: 80 })
      .toBuffer()

    const result = await blob.put(
      \`images/\${baseName}-\${variant.suffix}.webp\`,
      resized,
      { contentType: 'image/webp' }
    )
    results[variant.suffix] = result.url
  }

  return results
}`}
              />

              <ExampleSection
                title="Document Management"
                description="Organize and manage user documents"
                code={`import { createBlobClient } from '@temps-sdk/blob'

const blob = createBlobClient()

// Upload document with metadata in path
async function uploadDocument(
  userId: string,
  category: string,
  file: File
): Promise<string> {
  const date = new Date().toISOString().split('T')[0]
  const pathname = \`users/\${userId}/\${category}/\${date}/\${file.name}\`

  const buffer = Buffer.from(await file.arrayBuffer())
  const result = await blob.put(pathname, buffer, {
    contentType: file.type,
    addRandomSuffix: true,
  })

  return result.url
}

// List user's documents
async function listUserDocuments(
  userId: string,
  category?: string
): Promise<BlobInfo[]> {
  const prefix = category
    ? \`users/\${userId}/\${category}/\`
    : \`users/\${userId}/\`

  const allBlobs: BlobInfo[] = []
  let cursor: string | undefined

  do {
    const result = await blob.list({ prefix, cursor, limit: 100 })
    allBlobs.push(...result.blobs)
    cursor = result.cursor
  } while (cursor)

  return allBlobs
}

// Delete all documents for a user
async function deleteUserDocuments(userId: string): Promise<number> {
  const documents = await listUserDocuments(userId)
  if (documents.length === 0) return 0

  return blob.del(...documents.map(d => d.pathname))
}`}
              />

              <ExampleSection
                title="Streaming Large Files"
                description="Handle large file downloads efficiently with streaming"
                code={`import { createBlobClient } from '@temps-sdk/blob'
import { NextRequest, NextResponse } from 'next/server'

const blob = createBlobClient()

export async function GET(
  request: NextRequest,
  { params }: { params: { pathname: string } }
) {
  const pathname = params.pathname

  try {
    // Get metadata first
    const info = await blob.head(pathname)

    // Stream the file content
    const stream = await blob.download(pathname)

    return new NextResponse(stream, {
      headers: {
        'Content-Type': info.contentType,
        'Content-Length': info.size.toString(),
        'Content-Disposition': \`attachment; filename="\${pathname.split('/').pop()}"\`,
        'Cache-Control': 'public, max-age=31536000, immutable',
      },
    })
  } catch (error) {
    return NextResponse.json(
      { error: 'File not found' },
      { status: 404 }
    )
  }
}

// Progress tracking for uploads
async function uploadWithProgress(
  pathname: string,
  file: File,
  onProgress: (percent: number) => void
): Promise<BlobInfo> {
  // For large files, you might want to use multipart upload
  // This is a simplified example
  const buffer = Buffer.from(await file.arrayBuffer())
  onProgress(50) // Simulated progress

  const result = await blob.put(pathname, buffer)
  onProgress(100)

  return result
}`}
              />
            </CardContent>
          </Card>
        </TabsContent>
      </Tabs>
    </div>
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
        <p className="text-sm text-muted-foreground">{description}</p>
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
