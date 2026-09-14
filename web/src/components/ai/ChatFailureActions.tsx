// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link } from 'react-router'
import { importLocalAiProviderCredential, listAiProviders } from '@/api/client'
import { Button } from '@/components/ui/button'
import { canRefreshLocalCredential } from './chat-failure-recovery'

export function ChatFailureActions({ code, provider, busy, retryable, onRetry, onRefresh }: {
  code: string
  provider: string
  busy: boolean
  retryable: boolean
  onRetry?: () => void
  onRefresh: () => Promise<void>
}) {
  const [refreshing, setRefreshing] = useState(false)
  const [notice, setNotice] = useState<string | null>(null)
  const [credentialRefreshed, setCredentialRefreshed] = useState(false)
  async function refresh() {
    setRefreshing(true)
    setNotice(null)
    try {
      const { data } = await listAiProviders({ query: { catalog_only: false }, throwOnError: true })
      const current = data.providers.find((item) => item.id === provider)
      if (!current?.local_credential) {
        setNotice('No local login is available on the Temps server. Sign in there or update the saved credential in provider settings.')
        return
      }
      await importLocalAiProviderCredential({ path: { provider_id: provider }, throwOnError: true })
      await onRefresh()
      setCredentialRefreshed(true)
      setNotice('Local credential refreshed. Retry your message to use it; your workspace files are unchanged.')
    } catch {
      setNotice('Could not refresh the local credential. Open provider settings to check the login and your permission to update it.')
    } finally {
      setRefreshing(false)
    }
  }
  return <div className="mt-2 space-y-2">
    <div className="flex flex-wrap gap-2">
      {canRefreshLocalCredential(code, provider) && <Button size="sm" variant="outline" disabled={busy || refreshing} onClick={() => void refresh()}>
        {refreshing ? 'Refreshing local login…' : 'Refresh from local login'}
      </Button>}
      <Button size="sm" variant="outline" asChild><Link to={`/agent-sandbox/providers/${encodeURIComponent(provider)}`}>Provider settings</Link></Button>
      {code === 'provider_quota_exhausted' && <Button size="sm" variant="outline" asChild><Link to="/agent-sandbox/providers">Choose another provider</Link></Button>}
      {onRetry && <Button size="sm" variant="outline" title={!retryable && !credentialRefreshed ? 'Resolve the provider configuration or quota issue before retrying.' : undefined} disabled={busy || refreshing || (!retryable && !credentialRefreshed)} onClick={onRetry}>Retry message</Button>}
    </div>
    {notice && <p role="status" className="text-xs text-muted-foreground">{notice}</p>}
  </div>
}
