// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import {
  ArrowRight,
  CheckCircle2,
  Loader2,
  Sparkles,
  XCircle,
} from 'lucide-react'

import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { usePageTitle } from '@/hooks/usePageTitle'

interface CatalogEntry {
  id: string
  name: string
  install_command: string
  auth_command: string
  credential_saved: boolean
  current_auth_type: string | null
  default_model: string | null
}

interface CatalogResponse {
  default_provider: string
  providers: CatalogEntry[]
}

async function fetchCatalog(): Promise<CatalogResponse> {
  const r = await fetch('/api/settings/ai-providers')
  if (!r.ok) throw new Error(`Failed to load AI provider catalog (${r.status})`)
  return r.json()
}

// One row per provider. Configured providers collapse to a single line of
// status; unconfigured ones get a "Configure" CTA. Keeping this list dense
// because the user mostly just wants to see which provider is active.
export function AgentSandboxProvidersList() {
  usePageTitle('AI Providers')
  const { data, isPending, isError } = useQuery({
    queryKey: ['ai-provider-catalog'],
    queryFn: fetchCatalog,
    staleTime: 60 * 1000,
  })

  if (isPending) {
    return (
      <div className="flex justify-center py-12">
        <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (isError || !data) {
    return (
      <Card>
        <CardContent className="py-8 text-sm text-destructive">
          Failed to load AI provider catalog. Refresh the page to retry.
        </CardContent>
      </Card>
    )
  }

  return (
    <div className="space-y-4">
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <Sparkles className="h-4 w-4" />
            AI Providers
          </CardTitle>
          <CardDescription>
            Each provider has its own credential. The active provider is what
            workspaces and agents will use — saving Codex's API key won't
            replace your Claude subscription.
          </CardDescription>
        </CardHeader>
        <CardContent className="divide-y">
          {data.providers.map((p) => {
            const isActive = p.id === data.default_provider
            return (
              <div
                key={p.id}
                className="flex flex-col sm:flex-row sm:items-center gap-3 py-3 first:pt-0 last:pb-0"
              >
                <div className="flex items-center gap-3 min-w-0 flex-1">
                  {p.credential_saved ? (
                    <CheckCircle2 className="h-4 w-4 text-green-500 shrink-0" />
                  ) : (
                    <XCircle className="h-4 w-4 text-muted-foreground shrink-0" />
                  )}
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2 flex-wrap">
                      <p className="text-sm font-medium">{p.name}</p>
                      {isActive && (
                        <span className="inline-flex items-center rounded-full border border-primary/30 bg-primary/10 px-2 py-0.5 text-[11px] font-medium text-primary">
                          Active
                        </span>
                      )}
                      {p.credential_saved && (
                        <span className="inline-flex items-center gap-1 text-[11px] text-muted-foreground">
                          {p.current_auth_type === 'subscription'
                            ? 'OAuth token'
                            : p.current_auth_type === 'config_file'
                              ? 'Config file'
                              : 'API key'}
                        </span>
                      )}
                    </div>
                    <p className="text-xs text-muted-foreground truncate">
                      {p.credential_saved
                        ? p.default_model
                          ? `Model: ${p.default_model}`
                          : 'Model: provider default'
                        : 'No credential saved'}
                    </p>
                  </div>
                </div>
                <Button
                  asChild
                  variant="outline"
                  size="sm"
                  className="shrink-0"
                >
                  <Link to={`/agent-sandbox/providers/${p.id}`}>
                    {p.credential_saved ? 'Manage' : 'Configure'}
                    <ArrowRight className="h-3.5 w-3.5 ml-1.5" />
                  </Link>
                </Button>
              </div>
            )
          })}
        </CardContent>
      </Card>

      <p className="text-xs text-muted-foreground">
        Adding a new provider requires a Rust catalog entry on the server side —
        no UI configuration needed once the binary supports it.
      </p>
    </div>
  )
}
