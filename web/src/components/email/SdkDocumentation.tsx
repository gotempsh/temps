'use client'

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { CodeBlock } from '@/components/ui/code-block'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useQuery } from '@tanstack/react-query'
import {
  BookOpen,
  CheckCircle2,
  Code2,
  ExternalLink,
  Info,
  Package,
  Zap,
} from 'lucide-react'

// API function to check if we have providers/domains configured
async function getEmailStatus(): Promise<{
  hasProviders: boolean
  hasDomains: boolean
  verifiedDomains: string[]
}> {
  try {
    const [providersRes, domainsRes] = await Promise.all([
      fetch('/api/email-providers'),
      fetch('/api/email-domains'),
    ])

    const providers = providersRes.ok ? await providersRes.json() : []
    const domains = domainsRes.ok ? await domainsRes.json() : []

    return {
      hasProviders: providers.length > 0,
      hasDomains: domains.length > 0,
      verifiedDomains: domains
        .filter((d: { status: string }) => d.status === 'verified')
        .map((d: { domain: string }) => d.domain),
    }
  } catch {
    return { hasProviders: false, hasDomains: false, verifiedDomains: [] }
  }
}


function SetupStatus() {
  const { data: status } = useQuery({
    queryKey: ['email-status'],
    queryFn: getEmailStatus,
  })

  if (!status) return null

  const isReady = status.hasProviders && status.verifiedDomains.length > 0

  return (
    <Alert variant={isReady ? 'default' : 'destructive'}>
      {isReady ? (
        <CheckCircle2 className="h-4 w-4" />
      ) : (
        <Info className="h-4 w-4" />
      )}
      <AlertTitle>{isReady ? 'Ready to send emails' : 'Setup required'}</AlertTitle>
      <AlertDescription>
        {isReady ? (
          <>
            You have {status.verifiedDomains.length} verified domain
            {status.verifiedDomains.length > 1 ? 's' : ''}: {status.verifiedDomains.join(', ')}
          </>
        ) : (
          <>
            {!status.hasProviders && 'You need to configure an email provider. '}
            {status.hasProviders && !status.hasDomains && 'You need to add and verify a domain. '}
            {status.hasDomains && status.verifiedDomains.length === 0 && 'Your domains are pending DNS verification. '}
            Go to the Providers and Domains tabs to complete setup.
          </>
        )}
      </AlertDescription>
    </Alert>
  )
}

const installCode = `# Using npm
npm install @temps-sdk/node-sdk

# Using pnpm
pnpm add @temps-sdk/node-sdk

# Using bun
bun add @temps-sdk/node-sdk`

const basicUsageCode = `import { TempsClient } from '@temps-sdk/node-sdk';

// Initialize the client
const temps = new TempsClient({
  baseUrl: 'https://your-temps-instance.com',
  apiKey: process.env.TEMPS_API_KEY,
});

// Send a simple email
const { data, error } = await temps.email.send({
  body: {
    domain_id: 1,  // Your verified domain ID
    from: 'hello@mail.example.com',
    from_name: 'My App',
    to: ['user@example.com'],
    subject: 'Welcome to our platform!',
    html: '<h1>Hello World</h1><p>Welcome aboard!</p>',
    text: 'Hello World - Welcome aboard!',
    tags: ['welcome', 'onboarding'],
  }
});

if (error) {
  console.error('Failed to send email:', error);
} else {
  console.log('Email sent:', data.id);
}`

const reactEmailCode = `// emails/WelcomeEmail.tsx
import {
  Body,
  Container,
  Head,
  Heading,
  Html,
  Link,
  Preview,
  Text,
} from '@react-email/components';

interface WelcomeEmailProps {
  username: string;
  loginUrl: string;
}

export function WelcomeEmail({ username, loginUrl }: WelcomeEmailProps) {
  return (
    <Html>
      <Head />
      <Preview>Welcome to our platform, {username}!</Preview>
      <Body style={main}>
        <Container style={container}>
          <Heading style={h1}>Welcome, {username}!</Heading>
          <Text style={text}>
            We're excited to have you on board. Click the button below to get started.
          </Text>
          <Link href={loginUrl} style={button}>
            Get Started
          </Link>
        </Container>
      </Body>
    </Html>
  );
}

const main = {
  backgroundColor: '#f6f9fc',
  fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif',
};

const container = {
  backgroundColor: '#ffffff',
  margin: '40px auto',
  padding: '20px',
  borderRadius: '5px',
  maxWidth: '600px',
};

const h1 = {
  color: '#333',
  fontSize: '24px',
  fontWeight: 'bold',
};

const text = {
  color: '#666',
  fontSize: '16px',
  lineHeight: '24px',
};

const button = {
  backgroundColor: '#007bff',
  borderRadius: '5px',
  color: '#fff',
  display: 'inline-block',
  fontSize: '16px',
  fontWeight: 'bold',
  padding: '12px 24px',
  textDecoration: 'none',
};`

const reactEmailSendCode = `// Send the email using Temps SDK
import { render } from '@react-email/render';
import { TempsClient } from '@temps-sdk/node-sdk';
import { WelcomeEmail } from './emails/WelcomeEmail';

const temps = new TempsClient({
  baseUrl: 'https://your-temps-instance.com',
  apiKey: process.env.TEMPS_API_KEY,
});

// Render the React Email component to HTML
const html = await render(
  WelcomeEmail({
    username: 'John',
    loginUrl: 'https://app.example.com/login',
  })
);

// Send via Temps
const { data, error } = await temps.email.send({
  body: {
    domain_id: 1,
    from: 'hello@mail.example.com',
    from_name: 'My App',
    to: ['john@example.com'],
    subject: 'Welcome to our platform!',
    html,
    tags: ['welcome'],
  }
});

if (error) {
  console.error('Failed to send email:', error);
}`

const jsxEmailCode = `// emails/welcome.tsx
import {
  Body,
  Button,
  Container,
  Head,
  Heading,
  Html,
  Preview,
  Text,
} from 'jsx-email';

interface WelcomeProps {
  name: string;
  actionUrl: string;
}

export const Welcome = ({ name, actionUrl }: WelcomeProps) => (
  <Html>
    <Head />
    <Preview>Welcome to our app, {name}!</Preview>
    <Body style={{ backgroundColor: '#f4f4f5', padding: '20px' }}>
      <Container
        style={{
          backgroundColor: '#ffffff',
          padding: '40px',
          borderRadius: '8px',
          maxWidth: '600px',
        }}
      >
        <Heading style={{ color: '#18181b', marginBottom: '16px' }}>
          Welcome, {name}!
        </Heading>
        <Text style={{ color: '#71717a', fontSize: '16px', lineHeight: '24px' }}>
          Thanks for signing up. We're thrilled to have you.
        </Text>
        <Button
          href={actionUrl}
          style={{
            backgroundColor: '#2563eb',
            color: '#ffffff',
            padding: '12px 24px',
            borderRadius: '6px',
            fontWeight: '600',
            marginTop: '16px',
          }}
        >
          Get Started
        </Button>
      </Container>
    </Body>
  </Html>
);

export default Welcome;`

const jsxEmailSendCode = `// Send using jsx-email with Temps SDK
import { render } from 'jsx-email';
import { TempsClient } from '@temps-sdk/node-sdk';
import { Welcome } from './emails/welcome';

const temps = new TempsClient({
  baseUrl: 'https://your-temps-instance.com',
  apiKey: process.env.TEMPS_API_KEY,
});

// Render the jsx-email template
const html = await render(
  <Welcome name="Jane" actionUrl="https://app.example.com/dashboard" />
);

// Send via Temps
const { data, error } = await temps.email.send({
  body: {
    domain_id: 1,
    from: 'noreply@mail.example.com',
    from_name: 'My App',
    to: ['jane@example.com'],
    subject: 'Welcome to My App!',
    html,
    tags: ['welcome', 'new-user'],
  }
});

if (error) {
  console.error('Failed to send email:', error);
}`

const directApiCode = `// Direct API usage without SDK
const response = await fetch('https://your-temps-instance.com/api/emails', {
  method: 'POST',
  headers: {
    'Content-Type': 'application/json',
    'Authorization': 'Bearer YOUR_API_KEY',
  },
  body: JSON.stringify({
    domain_id: 1,
    from: 'hello@mail.example.com',
    from_name: 'My App',
    to: ['user@example.com'],
    subject: 'Hello from Temps!',
    html: '<h1>Hello!</h1><p>This is a test email.</p>',
    text: 'Hello! This is a test email.',
    tags: ['test'],
  }),
});

const result = await response.json();
console.log('Email sent:', result.id);`

const pythonCode = `import httpx

async def send_email():
    async with httpx.AsyncClient() as client:
        response = await client.post(
            "https://your-temps-instance.com/api/emails",
            headers={
                "Authorization": "Bearer YOUR_API_KEY",
                "Content-Type": "application/json",
            },
            json={
                "domain_id": 1,
                "from": "hello@mail.example.com",
                "from_name": "My App",
                "to": ["user@example.com"],
                "subject": "Hello from Python!",
                "html": "<h1>Hello!</h1>",
            }
        )
        return response.json()`

const goCode = `package main

import (
    "bytes"
    "encoding/json"
    "net/http"
)

type SendEmailRequest struct {
    DomainID  int      \`json:"domain_id"\`
    From      string   \`json:"from"\`
    FromName  string   \`json:"from_name"\`
    To        []string \`json:"to"\`
    Subject   string   \`json:"subject"\`
    HTML      string   \`json:"html"\`
}

func sendEmail() error {
    payload := SendEmailRequest{
        DomainID: 1,
        From:     "hello@mail.example.com",
        FromName: "My App",
        To:       []string{"user@example.com"},
        Subject:  "Hello from Go!",
        HTML:     "<h1>Hello!</h1>",
    }

    body, _ := json.Marshal(payload)
    req, _ := http.NewRequest("POST", "https://your-temps-instance.com/api/emails", bytes.NewBuffer(body))
    req.Header.Set("Authorization", "Bearer YOUR_API_KEY")
    req.Header.Set("Content-Type", "application/json")

    client := &http.Client{}
    resp, err := client.Do(req)
    if err != nil {
        return err
    }
    defer resp.Body.Close()

    return nil
}`

export function SdkDocumentation() {
  return (
    <div className="space-y-8">
      <div>
        <h2 className="text-2xl font-bold tracking-tight">SDK & Integration</h2>
        <p className="text-muted-foreground">
          Learn how to send transactional emails from your applications using the
          Temps SDK or direct API calls.
        </p>
      </div>

      <SetupStatus />

      {/* Quick Links */}
      <div className="grid gap-4 md:grid-cols-3">
        <Card className="cursor-pointer hover:border-primary/50 transition-colors">
          <CardHeader className="pb-2">
            <div className="flex items-center gap-2">
              <Package className="h-5 w-5 text-primary" />
              <CardTitle className="text-lg">@temps-sdk/node-sdk</CardTitle>
            </div>
          </CardHeader>
          <CardContent>
            <p className="text-sm text-muted-foreground">
              Official TypeScript/JavaScript SDK for Node.js and the browser.
            </p>
          </CardContent>
        </Card>

        <a
          href="https://react.email"
          target="_blank"
          rel="noopener noreferrer"
          className="block"
        >
          <Card className="cursor-pointer hover:border-primary/50 transition-colors h-full">
            <CardHeader className="pb-2">
              <div className="flex items-center gap-2">
                <Code2 className="h-5 w-5 text-pink-500" />
                <CardTitle className="text-lg">react-email</CardTitle>
                <ExternalLink className="h-4 w-4 text-muted-foreground ml-auto" />
              </div>
            </CardHeader>
            <CardContent>
              <p className="text-sm text-muted-foreground">
                Build beautiful emails using React components.
              </p>
            </CardContent>
          </Card>
        </a>

        <a
          href="https://jsx.email"
          target="_blank"
          rel="noopener noreferrer"
          className="block"
        >
          <Card className="cursor-pointer hover:border-primary/50 transition-colors h-full">
            <CardHeader className="pb-2">
              <div className="flex items-center gap-2">
                <Zap className="h-5 w-5 text-yellow-500" />
                <CardTitle className="text-lg">jsx-email</CardTitle>
                <ExternalLink className="h-4 w-4 text-muted-foreground ml-auto" />
              </div>
            </CardHeader>
            <CardContent>
              <p className="text-sm text-muted-foreground">
                High-performance JSX email templates with live preview.
              </p>
            </CardContent>
          </Card>
        </a>
      </div>

      {/* Installation */}
      <section className="space-y-4">
        <div className="flex items-center gap-2">
          <BookOpen className="h-5 w-5 text-muted-foreground" />
          <h3 className="text-xl font-semibold">Installation</h3>
        </div>
        <CodeBlock code={installCode} language="bash" title="Install the SDK" />
      </section>

      {/* Basic Usage */}
      <section className="space-y-4">
        <h3 className="text-xl font-semibold">Basic Usage</h3>
        <CodeBlock code={basicUsageCode} language="typescript" title="Send a simple email" />
      </section>

      {/* Framework Integration */}
      <section className="space-y-6">
        <h3 className="text-xl font-semibold">Email Templates</h3>
        <p className="text-muted-foreground">
          Build beautiful, type-safe email templates using React-based libraries.
          Choose between react-email and jsx-email based on your preferences.
        </p>

        <Tabs defaultValue="react-email" className="w-full">
          <TabsList>
            <TabsTrigger value="react-email" className="gap-2">
              <Code2 className="h-4 w-4" />
              react-email
            </TabsTrigger>
            <TabsTrigger value="jsx-email" className="gap-2">
              <Zap className="h-4 w-4" />
              jsx-email
            </TabsTrigger>
          </TabsList>

          <TabsContent value="react-email" className="space-y-4 mt-4">
            <div className="flex items-center gap-2">
              <Badge variant="secondary">@react-email/components</Badge>
              <span className="text-sm text-muted-foreground">
                Popular, mature ecosystem
              </span>
            </div>

            <CodeBlock
              code="npm install @react-email/components @react-email/render"
              language="bash"
              title="Install dependencies"
            />

            <CodeBlock
              code={reactEmailCode}
              language="typescript"
              title="Create email template (emails/WelcomeEmail.tsx)"
            />

            <CodeBlock
              code={reactEmailSendCode}
              language="typescript"
              title="Send with Temps SDK"
            />
          </TabsContent>

          <TabsContent value="jsx-email" className="space-y-4 mt-4">
            <div className="flex items-center gap-2">
              <Badge variant="secondary">jsx-email</Badge>
              <span className="text-sm text-muted-foreground">
                Modern, fast, great DX
              </span>
            </div>

            <CodeBlock
              code="npm install jsx-email"
              language="bash"
              title="Install jsx-email"
            />

            <CodeBlock
              code={jsxEmailCode}
              language="typescript"
              title="Create email template (emails/welcome.tsx)"
            />

            <CodeBlock code={jsxEmailSendCode} language="typescript" title="Send with Temps SDK" />
          </TabsContent>
        </Tabs>
      </section>

      {/* Direct API */}
      <section className="space-y-4">
        <h3 className="text-xl font-semibold">Direct API Usage</h3>
        <p className="text-muted-foreground">
          If you prefer not to use the SDK, you can call the API directly from any
          language.
        </p>

        <Tabs defaultValue="fetch" className="w-full">
          <TabsList>
            <TabsTrigger value="fetch">JavaScript</TabsTrigger>
            <TabsTrigger value="python">Python</TabsTrigger>
            <TabsTrigger value="go">Go</TabsTrigger>
          </TabsList>

          <TabsContent value="fetch" className="mt-4">
            <CodeBlock code={directApiCode} language="typescript" title="Using fetch" />
          </TabsContent>

          <TabsContent value="python" className="mt-4">
            <CodeBlock code={pythonCode} language="python" title="Using httpx" />
          </TabsContent>

          <TabsContent value="go" className="mt-4">
            <CodeBlock code={goCode} language="go" title="Using net/http" />
          </TabsContent>
        </Tabs>
      </section>

      {/* API Reference */}
      <section className="space-y-4">
        <h3 className="text-xl font-semibold">API Reference</h3>

        <Card>
          <CardHeader>
            <CardTitle className="text-lg font-mono">
              POST /api/emails
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <p className="text-sm text-muted-foreground">
              Send an email through a verified domain.
            </p>

            <div className="space-y-2">
              <h4 className="font-medium text-sm">Request Body</h4>
              <div className="rounded-md border overflow-hidden">
                <table className="w-full text-sm">
                  <thead className="bg-muted/50">
                    <tr>
                      <th className="text-left p-3">Field</th>
                      <th className="text-left p-3">Type</th>
                      <th className="text-left p-3">Required</th>
                      <th className="text-left p-3">Description</th>
                    </tr>
                  </thead>
                  <tbody>
                    <tr className="border-t">
                      <td className="p-3 font-mono">domain_id</td>
                      <td className="p-3">number</td>
                      <td className="p-3">Yes</td>
                      <td className="p-3 text-muted-foreground">ID of the verified domain</td>
                    </tr>
                    <tr className="border-t">
                      <td className="p-3 font-mono">from</td>
                      <td className="p-3">string</td>
                      <td className="p-3">Yes</td>
                      <td className="p-3 text-muted-foreground">Sender email address</td>
                    </tr>
                    <tr className="border-t">
                      <td className="p-3 font-mono">from_name</td>
                      <td className="p-3">string</td>
                      <td className="p-3">No</td>
                      <td className="p-3 text-muted-foreground">Sender display name</td>
                    </tr>
                    <tr className="border-t">
                      <td className="p-3 font-mono">to</td>
                      <td className="p-3">string[]</td>
                      <td className="p-3">Yes</td>
                      <td className="p-3 text-muted-foreground">Recipient email addresses</td>
                    </tr>
                    <tr className="border-t">
                      <td className="p-3 font-mono">cc</td>
                      <td className="p-3">string[]</td>
                      <td className="p-3">No</td>
                      <td className="p-3 text-muted-foreground">CC recipients</td>
                    </tr>
                    <tr className="border-t">
                      <td className="p-3 font-mono">bcc</td>
                      <td className="p-3">string[]</td>
                      <td className="p-3">No</td>
                      <td className="p-3 text-muted-foreground">BCC recipients</td>
                    </tr>
                    <tr className="border-t">
                      <td className="p-3 font-mono">reply_to</td>
                      <td className="p-3">string</td>
                      <td className="p-3">No</td>
                      <td className="p-3 text-muted-foreground">Reply-to address</td>
                    </tr>
                    <tr className="border-t">
                      <td className="p-3 font-mono">subject</td>
                      <td className="p-3">string</td>
                      <td className="p-3">Yes</td>
                      <td className="p-3 text-muted-foreground">Email subject line</td>
                    </tr>
                    <tr className="border-t">
                      <td className="p-3 font-mono">html</td>
                      <td className="p-3">string</td>
                      <td className="p-3">*</td>
                      <td className="p-3 text-muted-foreground">HTML email body</td>
                    </tr>
                    <tr className="border-t">
                      <td className="p-3 font-mono">text</td>
                      <td className="p-3">string</td>
                      <td className="p-3">*</td>
                      <td className="p-3 text-muted-foreground">Plain text email body</td>
                    </tr>
                    <tr className="border-t">
                      <td className="p-3 font-mono">headers</td>
                      <td className="p-3">object</td>
                      <td className="p-3">No</td>
                      <td className="p-3 text-muted-foreground">Custom email headers</td>
                    </tr>
                    <tr className="border-t">
                      <td className="p-3 font-mono">tags</td>
                      <td className="p-3">string[]</td>
                      <td className="p-3">No</td>
                      <td className="p-3 text-muted-foreground">Tags for categorization</td>
                    </tr>
                  </tbody>
                </table>
              </div>
              <p className="text-xs text-muted-foreground">
                * At least one of <code>html</code> or <code>text</code> is required.
              </p>
            </div>
          </CardContent>
        </Card>
      </section>
    </div>
  )
}
