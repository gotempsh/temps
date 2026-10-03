// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useState } from 'react'
import { Link, useNavigate } from 'react-router'
import { DropZone } from '@/components/drop/DropZone'
import { handOffDropFiles } from '@/lib/drop-handoff'
import { usePlatformFeatures } from '@/hooks/usePlatformFeatures'
import { sourceArchiveUploadsSupported } from '@/lib/platform-capabilities'

/** Share the real uploader and hand its File objects to the existing Drop flow. */
export function StarterLocalFiles() {
  const navigate = useNavigate()
  const features = usePlatformFeatures()
  const supported = sourceArchiveUploadsSupported(features.data)
  const [error, setError] = useState<string | null>(null)
  return (
    <div className="space-y-4">
      <div>
        <h3 className="text-sm font-semibold">From local files</h3>
        <p className="mt-1 text-sm text-muted-foreground">
          No Git account needed. Review the detected settings before deploying.
        </p>
      </div>
      <DropZone
        files={[]}
        disabled={!supported}
        onError={setError}
        onSelect={(files) => {
          if (!files.length) return
          setError(null)
          handOffDropFiles(files)
          navigate('/projects/new?source=drop')
        }}
      />
      {!supported && (
        <p role="status" className="text-sm text-muted-foreground">
          File uploads are unavailable on this stateless control plane.{' '}
          <Link
            className="underline underline-offset-4"
            to="/projects/new?source=manual"
          >
            Deploy a prebuilt image
          </Link>{' '}
          instead.
        </p>
      )}
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <p className="text-xs text-muted-foreground">
        Selecting files opens Drop files and uploads your packaged project for
        preset detection. Deployment starts only when you confirm.
      </p>
    </div>
  )
}
