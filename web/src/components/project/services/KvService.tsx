import { ProjectResponse } from '@/api/client'
import { kvStatusOptions } from '@/api/client/@tanstack/react-query.gen'
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
  Database,
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

interface KvServiceProps {
  project: ProjectResponse
}

export function KvService({ project: _project }: KvServiceProps) {
  const { setBreadcrumbs } = useBreadcrumbs()

  // Fetch KV status
  const { data: status, isLoading } = useQuery({
    ...kvStatusOptions(),
    refetchInterval: 10000, // Refetch every 10 seconds
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Services', href: `../services` },
      { label: 'KV Store' },
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
              <Database className="h-6 w-6 text-primary" />
            </div>
            <div>
              <h1 className="text-xl font-semibold sm:text-2xl">KV Store</h1>
              <p className="text-muted-foreground text-sm">
                Redis-backed key-value storage
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
            <Database className="h-6 w-6 text-primary" />
          </div>
          <div>
            <h1 className="text-xl font-semibold sm:text-2xl">KV Store</h1>
            <p className="text-muted-foreground text-sm">
              Redis-backed key-value storage
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
          {/* Status Card */}
          <Card>
            <CardHeader>
              <CardTitle>Service Status</CardTitle>
              <CardDescription>
                View your KV Store service status
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
                    <AlertTitle>KV Store is not enabled</AlertTitle>
                    <AlertDescription>
                      The KV Store service needs to be enabled by a system administrator.
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
                Install and use the Temps KV package in your TypeScript/JavaScript application
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              {/* Installation */}
              <div className="space-y-3">
                <h3 className="font-medium">Installation</h3>
                <div className="relative">
                  <pre className="bg-muted rounded-lg p-4 text-sm font-mono overflow-x-auto">
                    <code>npm install @temps-sdk/kv</code>
                  </pre>
                  <CopyButton
                    value="npm install @temps-sdk/kv"
                    className="absolute top-2 right-2 h-8 w-8 p-0 hover:bg-accent hover:text-accent-foreground rounded-md"
                  />
                </div>
                <p className="text-sm text-muted-foreground">
                  Or using other package managers:
                </p>
                <div className="grid gap-2 sm:grid-cols-3">
                  <CodeBlock code="yarn add @temps-sdk/kv" />
                  <CodeBlock code="pnpm add @temps-sdk/kv" />
                  <CodeBlock code="bun add @temps-sdk/kv" />
                </div>
              </div>

              {/* Configuration */}
              <div className="space-y-3">
                <h3 className="font-medium">Configuration</h3>
                <p className="text-sm text-muted-foreground">
                  The KV client automatically reads the <code className="bg-muted px-1.5 py-0.5 rounded text-xs">TEMPS_KV_URL</code> environment
                  variable which is injected into your project's runtime.
                </p>
                <CodeBlock
                  code={`import { createKvClient } from '@temps-sdk/kv'

// Automatically uses TEMPS_KV_URL from environment
const kv = createKvClient()

// Or configure manually
const kv = createKvClient({
  url: process.env.TEMPS_KV_URL,
  token: process.env.TEMPS_PROJECT_TOKEN,
})`}
                  language="typescript"
                />
              </div>

              {/* API Reference */}
              <div className="space-y-4">
                <h3 className="font-medium">API Reference</h3>

                <ApiMethod
                  name="get"
                  description="Get a value by key"
                  signature="kv.get<T>(key: string): Promise<T | null>"
                  example={`const user = await kv.get<User>('user:123')
if (user) {
  console.log(user.name)
}`}
                />

                <ApiMethod
                  name="set"
                  description="Set a value with optional expiration"
                  signature="kv.set(key: string, value: any, options?: SetOptions): Promise<void>"
                  example={`// Simple set
await kv.set('user:123', { name: 'John', email: 'john@example.com' })

// With expiration (TTL in seconds)
await kv.set('session:abc', sessionData, { ex: 3600 })

// Set only if key doesn't exist (NX)
await kv.set('lock:resource', '1', { nx: true, ex: 30 })`}
                />

                <ApiMethod
                  name="del"
                  description="Delete one or more keys"
                  signature="kv.del(...keys: string[]): Promise<number>"
                  example={`// Delete single key
await kv.del('user:123')

// Delete multiple keys
const deleted = await kv.del('cache:a', 'cache:b', 'cache:c')
console.log(\`Deleted \${deleted} keys\`)`}
                />

                <ApiMethod
                  name="incr / incrby"
                  description="Increment a numeric value atomically"
                  signature="kv.incr(key: string): Promise<number>
kv.incrby(key: string, amount: number): Promise<number>"
                  example={`// Increment by 1
const views = await kv.incr('page:views:home')

// Increment by specific amount
const score = await kv.incrby('user:123:score', 10)`}
                />

                <ApiMethod
                  name="expire"
                  description="Set expiration time on an existing key"
                  signature="kv.expire(key: string, seconds: number): Promise<boolean>"
                  example={`// Set key to expire in 1 hour
const success = await kv.expire('session:abc', 3600)`}
                />

                <ApiMethod
                  name="ttl"
                  description="Get remaining time-to-live for a key"
                  signature="kv.ttl(key: string): Promise<number>"
                  example={`const ttl = await kv.ttl('session:abc')
if (ttl < 300) {
  // Less than 5 minutes remaining, refresh
  await kv.expire('session:abc', 3600)
}`}
                />

                <ApiMethod
                  name="keys"
                  description="Find keys matching a pattern"
                  signature="kv.keys(pattern: string): Promise<string[]>"
                  example={`// Find all user keys
const userKeys = await kv.keys('user:*')

// Find all session keys for a user
const sessions = await kv.keys('session:user:123:*')`}
                />

                <ApiMethod
                  name="exists"
                  description="Check if a key exists"
                  signature="kv.exists(key: string): Promise<boolean>"
                  example={`if (await kv.exists('user:123')) {
  console.log('User exists')
}`}
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
                Common patterns and use cases for the KV Store
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              <ExampleSection
                title="Session Management"
                description="Store user sessions with automatic expiration"
                code={`import { createKvClient } from '@temps-sdk/kv'

const kv = createKvClient()

interface Session {
  userId: string
  email: string
  createdAt: string
}

// Create session (expires in 24 hours)
async function createSession(userId: string, email: string): Promise<string> {
  const sessionId = crypto.randomUUID()
  const session: Session = {
    userId,
    email,
    createdAt: new Date().toISOString(),
  }

  await kv.set(\`session:\${sessionId}\`, session, { ex: 86400 })
  return sessionId
}

// Get session
async function getSession(sessionId: string): Promise<Session | null> {
  return kv.get<Session>(\`session:\${sessionId}\`)
}

// Refresh session TTL
async function refreshSession(sessionId: string): Promise<boolean> {
  return kv.expire(\`session:\${sessionId}\`, 86400)
}

// Delete session (logout)
async function deleteSession(sessionId: string): Promise<void> {
  await kv.del(\`session:\${sessionId}\`)
}`}
              />

              <ExampleSection
                title="Rate Limiting"
                description="Implement API rate limiting with sliding window"
                code={`import { createKvClient } from '@temps-sdk/kv'

const kv = createKvClient()

interface RateLimitResult {
  allowed: boolean
  remaining: number
  resetAt: number
}

async function checkRateLimit(
  identifier: string,
  limit: number = 100,
  windowSeconds: number = 60
): Promise<RateLimitResult> {
  const key = \`ratelimit:\${identifier}\`

  // Increment counter
  const count = await kv.incr(key)

  // Set expiration on first request
  if (count === 1) {
    await kv.expire(key, windowSeconds)
  }

  const ttl = await kv.ttl(key)

  return {
    allowed: count <= limit,
    remaining: Math.max(0, limit - count),
    resetAt: Date.now() + ttl * 1000,
  }
}

// Usage in API route
export async function POST(request: Request) {
  const ip = request.headers.get('x-forwarded-for') || 'unknown'
  const result = await checkRateLimit(ip)

  if (!result.allowed) {
    return new Response('Too many requests', {
      status: 429,
      headers: {
        'X-RateLimit-Remaining': result.remaining.toString(),
        'X-RateLimit-Reset': result.resetAt.toString(),
      }
    })
  }

  // Process request...
}`}
              />

              <ExampleSection
                title="Caching"
                description="Cache expensive computations or API responses"
                code={`import { createKvClient } from '@temps-sdk/kv'

const kv = createKvClient()

async function cachedFetch<T>(
  key: string,
  fetcher: () => Promise<T>,
  ttlSeconds: number = 300
): Promise<T> {
  // Try to get from cache
  const cached = await kv.get<T>(key)
  if (cached !== null) {
    return cached
  }

  // Fetch fresh data
  const data = await fetcher()

  // Store in cache
  await kv.set(key, data, { ex: ttlSeconds })

  return data
}

// Usage
const user = await cachedFetch(
  \`user:\${userId}\`,
  () => fetchUserFromDatabase(userId),
  600 // Cache for 10 minutes
)`}
              />

              <ExampleSection
                title="Real-time Counters"
                description="Track page views, likes, or other metrics in real-time"
                code={`import { createKvClient } from '@temps-sdk/kv'

const kv = createKvClient()

// Track page view
async function trackPageView(pageSlug: string): Promise<number> {
  const key = \`pageviews:\${pageSlug}\`
  return kv.incr(key)
}

// Get view count
async function getPageViews(pageSlug: string): Promise<number> {
  const views = await kv.get<number>(\`pageviews:\${pageSlug}\`)
  return views || 0
}

// Track daily unique visitors
async function trackDailyVisitor(pageSlug: string, visitorId: string): Promise<void> {
  const today = new Date().toISOString().split('T')[0]
  const key = \`visitors:\${pageSlug}:\${today}:\${visitorId}\`

  // Set with expiration at end of day (24 hours)
  await kv.set(key, '1', { nx: true, ex: 86400 })
}

// Get daily unique visitor count
async function getDailyVisitorCount(pageSlug: string): Promise<number> {
  const today = new Date().toISOString().split('T')[0]
  const keys = await kv.keys(\`visitors:\${pageSlug}:\${today}:*\`)
  return keys.length
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
