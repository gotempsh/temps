// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { AlertTriangle } from 'lucide-react'
import { Link } from 'react-router'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { usePublicIngress } from '@/hooks/usePublicIngress'

interface PublicIngressNoticeProps {
  /** What stops working on this page, e.g. "Opens and clicks are not recorded". */
  title: string
  className?: string
}

/**
 * Warns when Temps can tell that third parties cannot reach the URL they are
 * given (ADR-051). Without it the only symptom is data that never arrives —
 * an Opens column that stays at zero, a push that deploys nothing — because
 * the failing request never reaches Temps to be reported.
 *
 * Renders nothing when callbacks can arrive, or when the status is unknown.
 */
export function PublicIngressNotice({
  title,
  className,
}: PublicIngressNoticeProps) {
  const { status } = usePublicIngress()

  if (!status || status.configured) return null

  return (
    <Alert variant="warning" className={className}>
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription className="space-y-2">
        <p>{status.reason}</p>
        {status.setup_path && (
          <p>
            <Link
              to={status.setup_path}
              className="font-medium underline underline-offset-4"
            >
              Set a public ingress URL
            </Link>
          </p>
        )}
      </AlertDescription>
    </Alert>
  )
}
