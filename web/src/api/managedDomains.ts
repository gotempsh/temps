// Hand-written wrappers for managed-domain endpoints not yet present in the
// generated client (`web/src/api/client`). Regenerating the client with
// `bun openapi-ts` against a running backend will add typed equivalents; until
// then these thin wrappers call the same fetch client used by the generated SDK.
import { client } from '@/api/client/client.gen'
import type {
  HostnamePreviewResponse,
  ManagedDomainResponse,
} from '@/api/client'

export interface UpdateManagedDomainBody {
  generated_hostname_mode?: string
  sync_generated_records?: boolean
  auto_manage?: boolean
}

export async function updateManagedDomainSettings(
  providerId: number,
  domain: string,
  body: UpdateManagedDomainBody
): Promise<ManagedDomainResponse> {
  const res = await client.patch({
    security: [{ scheme: 'bearer', type: 'http' }],
    url: '/dns-providers/{provider_id}/domains/{domain}',
    path: { provider_id: providerId, domain },
    body,
  })
  if (res.error) throw res.error
  return res.data as unknown as ManagedDomainResponse
}

export async function previewHostnameMode(
  providerId: number,
  domain: string,
  mode: 'standard' | 'flat',
  sync: boolean
): Promise<HostnamePreviewResponse> {
  const res = await client.get({
    security: [{ scheme: 'bearer', type: 'http' }],
    url: '/dns-providers/{provider_id}/domains/{domain}/hostname-preview',
    path: { provider_id: providerId, domain },
    query: { mode, sync },
  })
  if (res.error) throw res.error
  return res.data as unknown as HostnamePreviewResponse
}

export async function applyHostnameMode(
  providerId: number,
  domain: string,
  mode: 'standard' | 'flat',
  syncDns: boolean
): Promise<HostnamePreviewResponse> {
  const res = await client.post({
    security: [{ scheme: 'bearer', type: 'http' }],
    url: '/dns-providers/{provider_id}/domains/{domain}/apply-hostname-mode',
    path: { provider_id: providerId, domain },
    body: { mode, sync_dns: syncDns },
  })
  if (res.error) throw res.error
  return res.data as unknown as HostnamePreviewResponse
}
