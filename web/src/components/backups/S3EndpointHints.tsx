// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import {
  describePathStyleReason,
  previewPathStyle,
  previewS3Endpoint,
} from '@/lib/s3-endpoint'

/** One line under the endpoint field: the URL every client will receive. */
export function S3EndpointPreviewLine({
  endpoint,
}: {
  endpoint: string | null | undefined
}) {
  const preview = previewS3Endpoint(endpoint)
  if (!(endpoint ?? '').trim()) return null
  if (!preview) {
    return (
      <p className="text-xs text-destructive">
        Enter a host such as 10.0.0.5:9000 or a URL starting with http:// or
        https://.
      </p>
    )
  }
  return (
    <p
      className="text-xs text-muted-foreground [overflow-wrap:anywhere]"
      data-testid="s3-endpoint-preview"
    >
      Backups and WAL archiving connect to{' '}
      <span className="font-mono">{preview.url}</span>
      {preview.scheme === 'http' ? ' over plain HTTP' : ' over TLS'}
      {preview.explicitScheme ? '.' : '. Type https:// to use TLS.'}
    </p>
  )
}

/**
 * The path-style checkbox with the value clients will actually use. When an
 * IP endpoint or a managed store forces it, the box is shown ticked and
 * locked, with the reason, so an unticked box can never silently produce
 * `<bucket>.<ip>` requests.
 */
export function S3PathStyleField({
  id,
  endpoint,
  checked,
  onCheckedChange,
  managedObjectStore = false,
}: {
  id: string
  endpoint: string | null | undefined
  checked: boolean
  onCheckedChange: (checked: boolean) => void
  managedObjectStore?: boolean
}) {
  const effective = previewPathStyle(endpoint, checked, managedObjectStore)
  const forced = effective.reason !== 'configured'
  return (
    <Label htmlFor={id} className="flex items-center space-x-2">
      <Checkbox
        id={id}
        checked={effective.forcePathStyle}
        disabled={forced}
        onCheckedChange={(value) => onCheckedChange(value === true)}
      />
      <div>
        <span>Force path style</span>
        <p
          className="text-xs text-muted-foreground"
          data-testid="s3-path-style-reason"
        >
          {forced
            ? describePathStyleReason(effective.reason)
            : 'Enable for RustFS/MinIO compatibility'}
        </p>
      </div>
    </Label>
  )
}
