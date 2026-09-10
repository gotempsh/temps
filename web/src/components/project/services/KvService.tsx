// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
  Zap,
  Lock,
  Clock,
  Hash,
} from 'lucide-react'
import { Skeleton } from '@/components/ui/skeleton'
import { CopyButton } from '@/components/ui/copy-button'
import { Link } from 'react-router'

interface KvServiceProps {
  project: ProjectResponse
}

export function KvService({ project: _project }: KvServiceProps) {
  const { setBreadcrumbs } = useBreadcrumbs()

  const { data: status, isLoading } = useQuery({
    ...kvStatusOptions(),
    refetchInterval: 10000,
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Databases', href: `../storage` },
      { label: 'KV Store' },
    ])
  }, [setBreadcrumbs])

  const isEnabled = status?.enabled ?? false

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
                Serverless key-value store backed by Redis
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
            <Database className="h-6 w-6 text-primary" />
          </div>
          <div className="min-w-0">
            <h1 className="text-xl font-semibold sm:text-2xl">KV Store</h1>
            <p className="text-muted-foreground text-sm">
              Serverless key-value store backed by Redis — no infrastructure to manage
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
                Cluster-wide status of the KV service. Once enabled, every project on this instance can use it through the SDK below.
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
                      <p className="font-medium mt-1">Redis {status?.version || 'unknown'}</p>
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
                    <AlertTitle>KV Store is not enabled</AlertTitle>
                    <AlertDescription>
                      An administrator must enable the KV service from <strong>Storage Settings → Platform Services</strong>. Once enabled, the SDK on the Documentation tab works out of the box — no further per-project setup needed.
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
              icon={Zap}
              title="In-memory speed"
              description="Sub-millisecond reads and writes for the hot path of your application — caching, sessions, counters, feature flags."
            />
            <FeatureCard
              icon={Clock}
              title="TTL & expiration"
              description="Set a per-key TTL in seconds or milliseconds. Keys disappear automatically — no cleanup jobs to write."
            />
            <FeatureCard
              icon={Hash}
              title="Atomic counters"
              description="INCR is atomic across concurrent callers. Build rate limiters, view counters, and quotas without races."
            />
            <FeatureCard
              icon={Lock}
              title="Distributed locks"
              description="Combine SET NX with TTL to coordinate work across replicas. The SDK returns null when a lock is already held."
            />
          </div>
        </TabsContent>

        <TabsContent value="docs" className="space-y-6">
          <Card>
            <CardHeader>
              <CardTitle>TypeScript SDK</CardTitle>
              <CardDescription>
                The <code className="bg-muted px-1.5 py-0.5 rounded text-xs">@temps-sdk/kv</code> package
                gives you a typed client for KV operations from any Node.js or Bun runtime.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              <div className="space-y-3">
                <h3 className="font-medium">Installation</h3>
                <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
                  <CodeBlock code="npm install @temps-sdk/kv" />
                  <CodeBlock code="bun add @temps-sdk/kv" />
                  <CodeBlock code="pnpm add @temps-sdk/kv" />
                  <CodeBlock code="yarn add @temps-sdk/kv" />
                </div>
              </div>

              <div className="space-y-3">
                <h3 className="font-medium">Quick start</h3>
                <p className="text-sm text-muted-foreground">
                  The default <code className="bg-muted px-1.5 py-0.5 rounded text-xs">kv</code> singleton
                  reads its config from environment variables — no extra wiring required when running
                  on Temps.
                </p>
                <CodeBlock
                  code={`import { kv } from '@temps-sdk/kv'

await kv.set('user:123', { name: 'Alice', plan: 'pro' })

const user = await kv.get<{ name: string; plan: string }>('user:123')
// { name: 'Alice', plan: 'pro' }

await kv.del('user:123')`}
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
                  code={`import { createClient } from '@temps-sdk/kv'

const kv = createClient({
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
                  name="get"
                  description="Retrieve a value by key. Returns null if the key does not exist."
                  signature="get<T = unknown>(key: string): Promise<T | null>"
                  example={`const session = await kv.get<{ userId: string; expiresAt: number }>('session:abc')

if (session) {
  console.log(session.userId)
}`}
                />

                <ApiMethod
                  name="set"
                  description="Store a JSON-serializable value. Returns 'OK' on success, or null when a conditional write (nx / xx) was rejected."
                  signature={`set(
  key: string,
  value: unknown,
  options?: {
    ex?: number   // expire after N seconds
    px?: number   // expire after N milliseconds
    nx?: boolean  // only set if key does NOT exist
    xx?: boolean  // only set if key already exists
  },
): Promise<'OK' | null>`}
                  example={`// Simple set
await kv.set('config:theme', 'dark')

// Expire after 5 minutes
await kv.set('cache:homepage', html, { ex: 300 })

// Create-if-missing (returns null if key already exists)
const acquired = await kv.set('lock:deploy', '1', { nx: true, ex: 30 })
if (acquired === null) console.log('Lock already held')

// Update-if-exists
await kv.set('user:123:status', 'active', { xx: true })`}
                />

                <ApiMethod
                  name="del"
                  description="Delete one or more keys. Returns the number of keys that were actually removed."
                  signature="del(...keys: string[]): Promise<number>"
                  example={`const removed = await kv.del('temp:a', 'temp:b', 'temp:c')
console.log(\`Removed \${removed} keys\`)`}
                />

                <ApiMethod
                  name="incr"
                  description="Atomically increment a numeric value by 1. Initializes the key to 0 first if it does not exist."
                  signature="incr(key: string): Promise<number>"
                  example={`const views = await kv.incr('page:views:/pricing')
console.log(\`Page views: \${views}\`)`}
                />

                <ApiMethod
                  name="expire"
                  description="Set a TTL on an existing key. Returns 1 if the timeout was set, 0 if the key does not exist."
                  signature="expire(key: string, seconds: number): Promise<number>"
                  example={`await kv.set('session:xyz', data)
await kv.expire('session:xyz', 3600) // expire in 1 hour`}
                />

                <ApiMethod
                  name="ttl"
                  description={`Get the remaining time-to-live for a key in seconds.

Returns:
  >= 0  – seconds remaining
  -1    – key exists but has no expiration
  -2    – key does not exist`}
                  signature="ttl(key: string): Promise<number>"
                  example={`const remaining = await kv.ttl('session:xyz')

if (remaining === -2) {
  console.log('Session expired or never existed')
} else if (remaining === -1) {
  console.log('Session has no expiration')
} else {
  console.log(\`Session expires in \${remaining}s\`)
}`}
                />

                <ApiMethod
                  name="keys"
                  description="Find all keys matching a glob-style pattern. Use sparingly in production — prefer direct key access for hot paths."
                  signature="keys(pattern: string): Promise<string[]>"
                  example={`const userKeys = await kv.keys('user:*')
// ['user:123', 'user:456', 'user:789']`}
                />
              </div>

              <div className="space-y-3">
                <h3 className="font-medium">Error handling</h3>
                <p className="text-sm text-muted-foreground">
                  Every error thrown by the SDK is an instance of{' '}
                  <code className="bg-muted px-1.5 py-0.5 rounded text-xs">KVError</code> with
                  structured fields you can branch on.
                </p>
                <CodeBlock
                  code={`import { kv, KVError } from '@temps-sdk/kv'

try {
  await kv.get('my-key')
} catch (error) {
  if (error instanceof KVError) {
    console.error(error.message) // human-readable
    console.error(error.code)    // 'MISSING_CONFIG' | 'NETWORK_ERROR' | http status
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
                Battle-tested recipes for the most common KV use cases.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              <ExampleSection
                title="Caching expensive lookups"
                description="Read-through cache: try KV first, fall back to the source of truth, then warm the cache for next time."
                code={`import { kv } from '@temps-sdk/kv'

async function getProduct(id: string) {
  const cached = await kv.get<Product>(\`product:\${id}\`)
  if (cached) return cached

  const product = await db.products.findById(id)
  await kv.set(\`product:\${id}\`, product, { ex: 600 }) // cache for 10 minutes
  return product
}`}
              />

              <ExampleSection
                title="Rate limiting"
                description="Fixed-window rate limiter using INCR + TTL. The first request seeds the counter and the window expiration."
                code={`import { kv } from '@temps-sdk/kv'

async function checkRateLimit(userId: string): Promise<boolean> {
  const key = \`ratelimit:\${userId}\`
  const count = await kv.incr(key)

  // First request in the window — set the expiration
  if (count === 1) {
    await kv.expire(key, 60)
  }

  return count <= 100 // 100 requests per 60-second window
}`}
              />

              <ExampleSection
                title="Distributed locks"
                description="Use SET NX with a TTL so a crashed worker can never deadlock the system."
                code={`import { kv } from '@temps-sdk/kv'

async function withLock<T>(
  name: string,
  fn: () => Promise<T>,
): Promise<T | null> {
  const acquired = await kv.set(\`lock:\${name}\`, Date.now(), {
    nx: true,
    ex: 30,
  })

  if (acquired === null) return null // lock held by another process

  try {
    return await fn()
  } finally {
    await kv.del(\`lock:\${name}\`)
  }
}

// Usage
await withLock('nightly-report', async () => {
  await generateReport()
})`}
              />

              <ExampleSection
                title="Sessions with rolling TTL"
                description="Issue session IDs in your auth flow, refresh the TTL on every authenticated request, and delete on logout."
                code={`import { kv } from '@temps-sdk/kv'

interface Session {
  userId: string
  email: string
  createdAt: string
}

const SESSION_TTL = 86400 // 24 hours

export async function createSession(userId: string, email: string) {
  const sessionId = crypto.randomUUID()
  await kv.set<Session>(
    \`session:\${sessionId}\`,
    { userId, email, createdAt: new Date().toISOString() },
    { ex: SESSION_TTL },
  )
  return sessionId
}

export async function getSession(sessionId: string) {
  const session = await kv.get<Session>(\`session:\${sessionId}\`)
  if (session) {
    // Roll the TTL forward on every successful read
    await kv.expire(\`session:\${sessionId}\`, SESSION_TTL)
  }
  return session
}

export async function destroySession(sessionId: string) {
  await kv.del(\`session:\${sessionId}\`)
}`}
              />

              <ExampleSection
                title="Feature flags"
                description="Store feature configuration in a single key and roll out gradually."
                code={`import { kv } from '@temps-sdk/kv'

interface FeatureFlag {
  enabled: boolean
  rollout: number // 0..1
}

await kv.set<FeatureFlag>('feature:dark-mode', {
  enabled: true,
  rollout: 0.5,
})

export async function isEnabled(name: string): Promise<boolean> {
  const flag = await kv.get<FeatureFlag>(\`feature:\${name}\`)
  if (!flag?.enabled) return false
  return Math.random() < flag.rollout
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
  icon: typeof Zap
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
