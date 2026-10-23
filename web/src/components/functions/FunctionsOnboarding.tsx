// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { CheckCircle2, CircleSlash, Zap } from 'lucide-react'
import { Link } from 'react-router'
import { Badge } from '@/components/ui/badge'
import {
  FUNCTIONS_SETUP_PATH,
  type FunctionsCapability,
  type RuntimeAvailability,
} from '@/lib/functions'

function RuntimeRow({
  title,
  availability,
  fix,
}: {
  title: string
  availability: RuntimeAvailability
  fix: ReactNode
}) {
  return (
    <li className="flex flex-col gap-1 p-3 sm:flex-row sm:items-start sm:gap-3">
      <div className="flex shrink-0 items-center gap-2 sm:w-48">
        {availability.available ? (
          <CheckCircle2
            className="h-4 w-4 text-success-strong"
            aria-hidden="true"
          />
        ) : (
          <CircleSlash
            className="h-4 w-4 text-muted-foreground"
            aria-hidden="true"
          />
        )}
        <span className="font-medium">{title}</span>
        <Badge
          variant={availability.available ? 'success-subtle' : 'neutral-subtle'}
        >
          {availability.available ? 'Available' : 'Unavailable'}
        </Badge>
      </div>
      <p className="min-w-0 text-sm text-muted-foreground">
        {availability.available ? (
          'Functions of this kind can be created.'
        ) : (
          <>
            {availability.reason ?? 'No runtime on the control plane runs it.'}{' '}
            {fix}
          </>
        )}
      </p>
    </li>
  )
}

/** Each runtime's availability, with what to do when it is missing. */
export function FunctionRuntimes({
  capability,
}: {
  capability: FunctionsCapability
}) {
  return (
    <ul className="divide-y rounded-lg border">
      <RuntimeRow
        title="WASM modules"
        availability={capability.wasm}
        fix={
          <>
            The WASM runtime is compiled into temps only with the{' '}
            <code>wasm</code> cargo feature, which is off by default: build with{' '}
            <code>cargo build --release --bin temps --features wasm</code> and
            restart.
          </>
        }
      />
      <RuntimeRow
        title="OCI images"
        availability={capability.oci}
        fix="An OCI function needs a container runtime on the control plane that can confine its egress."
      />
    </ul>
  )
}

/**
 * What functions do and what is missing on this instance (Feature
 * Discoverability): shown instead of the create action while no runtime can
 * run a function, never instead of the page.
 */
export function FunctionsOnboarding({
  capability,
  showSetupLink = true,
}: {
  capability: FunctionsCapability
  showSetupLink?: boolean
}) {
  const setupPath = capability.setup_path ?? FUNCTIONS_SETUP_PATH
  return (
    <section
      aria-label="Set up functions"
      className="space-y-4 rounded-lg border bg-card p-4 text-card-foreground"
    >
      <div className="flex items-start gap-3">
        <div className="flex size-9 shrink-0 items-center justify-center rounded-full bg-muted">
          <Zap className="h-4 w-4 text-muted-foreground" aria-hidden="true" />
        </div>
        <div className="min-w-0 space-y-1">
          <h2 className="text-base font-semibold">
            Run code only while requests arrive
          </h2>
          <p className="text-sm text-muted-foreground">
            Upload a WASM module or name an OCI image and it gets its own
            hostname. For example, upload <code>resize-image.wasm</code> and
            send requests to{' '}
            <code>
              https://fn-resize-image-your-project.your-preview-domain
            </code>
            : the function starts on the first request and sleeps after five
            idle minutes.
          </p>
        </div>
      </div>
      <p className="text-sm font-medium" role="status">
        {capability.reason ??
          'No runtime on the control plane can run a function yet.'}
      </p>
      <FunctionRuntimes capability={capability} />
      {showSetupLink ? (
        <p className="text-sm">
          <Link to={setupPath} className="underline underline-offset-2">
            See how to enable a function runtime
          </Link>
        </p>
      ) : null}
    </section>
  )
}
