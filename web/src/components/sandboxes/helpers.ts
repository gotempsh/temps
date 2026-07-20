import type {
  SandboxInner,
  SandboxResponse,
  JobSummaryResponse,
} from '@/api/client/types.gen'

export type SandboxView = {
  id: string
  name: string
  status: string
  image: string | null
  backend?: string | null
  disk_size_mb?: number | null
  work_dir: string
  created_at: string
  expires_at: string
  preview_url_template: string
  preview_password_hint?: string | null
}

export type JobSummary = JobSummaryResponse

// Flatten `@vercel/sandbox`-compatible `{ sandbox, routes }` into the
// view shape our UI has always consumed. Also translate epoch-ms
// `createdAt` + idle `timeout` (ms) into ISO `created_at` / `expires_at`
// so existing `new Date(...)` call sites keep working unchanged.
export function toSandboxView(inner: SandboxInner): SandboxView {
  const createdMs = inner.createdAt
  const expiresMs = createdMs + inner.timeout
  return {
    id: inner.id,
    name: inner.name,
    status: inner.status,
    image: inner.image ?? null,
    backend: inner.backend ?? null,
    disk_size_mb: inner.disk_size_mb ?? null,
    work_dir: inner.cwd,
    created_at: new Date(createdMs).toISOString(),
    expires_at: new Date(expiresMs).toISOString(),
    preview_url_template: inner.preview_url_template,
    preview_password_hint: inner.preview_password_hint ?? undefined,
  }
}

export function sandboxFromResponse(resp: SandboxResponse): SandboxView {
  return toSandboxView(resp.sandbox)
}

export function jobLogsUrl(sandboxId: string, jobId: string): string {
  return `/api/v1/sandboxes/${encodeURIComponent(sandboxId)}/jobs/${encodeURIComponent(jobId)}/logs`
}
