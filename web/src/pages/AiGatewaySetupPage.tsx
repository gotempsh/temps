import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { CopyButton } from '@/components/ui/copy-button'
import { CodeBlock } from '@/components/ui/code-block'
import { CodeTabs, type CodeExample } from '@/components/ui/code-tabs'
import { listProviderKeys, type ProviderKeyResponse } from '@/api/client'
import { useSettings } from '@/hooks/useSettings'
import { usePageTitle } from '@/hooks/usePageTitle'
import { AI_PROVIDERS, AiProviderIcon } from '@/lib/ai-providers'

const SUPPORTED_PROVIDERS = AI_PROVIDERS

// The "how do I actually call this" page — split out from AiGatewayPage's
// old Settings tab so it reads as documentation you land on once, not a
// tab buried behind Provider Keys/Usage/Activity.
export function AiGatewaySetupPage() {
  usePageTitle('AI Gateway Setup')

  const { data: settings } = useSettings()
  const externalUrl = settings?.external_url || window.location.origin
  const gatewayEndpoint = `${externalUrl}/api/ai/v1`

  const { data: keysData } = useQuery({
    queryKey: ['providerKeys'],
    queryFn: async () => {
      const response = await listProviderKeys()
      return response.data
    },
  })
  const keys: ProviderKeyResponse[] = keysData ?? []
  const firstConfiguredProvider = SUPPORTED_PROVIDERS.find((p) =>
    keys.some((k) => k.provider === p.id && k.is_active)
  )

  const [snippetLang, setSnippetLang] = useState<
    'bash' | 'python' | 'typescript'
  >('bash')
  const [snippetProvider, setSnippetProvider] = useState<string>('')

  const effectiveSnippetProvider =
    snippetProvider || firstConfiguredProvider?.id || SUPPORTED_PROVIDERS[0].id
  const snippetModel =
    SUPPORTED_PROVIDERS.find((p) => p.id === effectiveSnippetProvider)
      ?.defaultModel ?? SUPPORTED_PROVIDERS[0].defaultModel

  const codeSnippets = {
    bash: `curl ${gatewayEndpoint}/chat/completions \\
  -H "Authorization: Bearer YOUR_API_KEY" \\
  -H "Content-Type: application/json" \\
  -d '{
    "model": "${snippetModel}",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'`,
    python: `from openai import OpenAI

client = OpenAI(
    base_url="${gatewayEndpoint}",
    api_key="YOUR_API_KEY",
)

response = client.chat.completions.create(
    model="${snippetModel}",
    messages=[{"role": "user", "content": "Hello!"}],
)
print(response.choices[0].message.content)`,
    typescript: `import OpenAI from "openai";

const client = new OpenAI({
  baseURL: "${gatewayEndpoint}",
  apiKey: "YOUR_API_KEY",
});

const response = await client.chat.completions.create({
  model: "${snippetModel}",
  messages: [{ role: "user", content: "Hello!" }],
});
console.log(response.choices[0].message.content);`,
  }

  const snippetExamples: CodeExample[] = [
    { id: 'bash', label: 'cURL', language: 'bash', code: codeSnippets.bash },
    {
      id: 'python',
      label: 'Python',
      language: 'python',
      code: codeSnippets.python,
    },
    {
      id: 'typescript',
      label: 'Node.js',
      language: 'typescript',
      code: codeSnippets.typescript,
    },
  ]

  return (
    <div className="container mx-auto px-4 sm:px-6 py-4 sm:py-6 space-y-4 sm:space-y-6">
      <div>
        <h1 className="text-2xl sm:text-3xl font-bold">AI Gateway Setup</h1>
        <p className="text-muted-foreground mt-1 sm:mt-2 text-sm">
          One OpenAI-compatible endpoint for every configured provider.
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Gateway Endpoint</CardTitle>
          <CardDescription>
            Use this endpoint with any OpenAI-compatible SDK. Just swap the base
            URL and use your Temps API key.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="flex items-center gap-2">
            <code className="flex-1 rounded-md bg-muted px-3 py-2 text-sm font-mono">
              {gatewayEndpoint}
            </code>
            <CopyButton value={gatewayEndpoint} className="shrink-0" />
          </div>
          <p className="text-xs text-muted-foreground">
            The gateway is OpenAI-compatible — use any model from any configured
            provider with the same endpoint.
          </p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Quick Start</CardTitle>
          <CardDescription>
            Copy a code snippet to start making requests.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <CodeTabs
            value={snippetLang}
            onValueChange={(id) =>
              setSnippetLang(id as 'bash' | 'python' | 'typescript')
            }
            examples={snippetExamples}
            rightSlot={
              <Select
                value={effectiveSnippetProvider}
                onValueChange={setSnippetProvider}
              >
                <SelectTrigger className="h-8 w-full sm:w-[200px]">
                  <SelectValue placeholder="Provider" />
                </SelectTrigger>
                <SelectContent>
                  {SUPPORTED_PROVIDERS.map((p) => (
                    <SelectItem key={p.id} value={p.id}>
                      <div className="flex items-center gap-2">
                        <AiProviderIcon provider={p.id} size={20} />
                        <span>{p.name}</span>
                      </div>
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            }
          />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Bring Your Own Key (BYOK)</CardTitle>
        </CardHeader>
        <CardContent className="text-sm text-muted-foreground space-y-3">
          <p>
            You can also pass provider keys per-request using HTTP headers,
            bypassing the stored keys above. This is useful for testing or when
            you want to use a different key for specific requests.
          </p>
          <CodeBlock
            code={`X-Provider-Api-Key: sk-your-key-here\nX-Provider-Base-Url: https://custom-endpoint.example.com/v1`}
            language="text"
            title="BYOK Headers"
          />
          <p className="text-xs">
            When BYOK headers are present, stored keys are not used for that
            request. The response will include a{' '}
            <code className="bg-muted px-1 py-0.5 rounded text-foreground">
              x-temps-credential-type: byok
            </code>{' '}
            header.
          </p>
        </CardContent>
      </Card>
    </div>
  )
}
