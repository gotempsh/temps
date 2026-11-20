// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'
import { Gauge } from 'lucide-react'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import {
  DETECTOR_ADVICE,
  type PerformanceEvidence,
} from './performance-evidence'

/**
 * The evidence behind a performance issue (ADR 111, D2): what the detector
 * found in which operation, how often and for how long, and the trace it
 * last found it in.
 */
export function PerformanceIssueCard({
  projectSlug,
  title,
  evidence,
}: {
  projectSlug: string
  title: string
  evidence: PerformanceEvidence
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <Gauge className="h-4 w-4" />
          {title}
        </CardTitle>
        <CardDescription>
          Found by a trace detector in {evidence.operation || 'an operation'}
          {evidence.service ? ` of ${evidence.service}` : ''}.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3 text-sm">
        <pre className="whitespace-pre-wrap break-all rounded-md bg-muted p-3 font-mono text-xs">
          {evidence.pattern}
        </pre>
        <dl className="grid grid-cols-2 gap-2 sm:grid-cols-3">
          <div>
            <dt className="text-xs text-muted-foreground">Spans involved</dt>
            <dd className="font-medium">{evidence.occurrences}</dd>
          </div>
          <div>
            <dt className="text-xs text-muted-foreground">Time spent</dt>
            <dd className="font-medium">{evidence.durationMs.toFixed(1)} ms</dd>
          </div>
        </dl>
        {DETECTOR_ADVICE[evidence.detector] && (
          <p className="text-muted-foreground">
            {DETECTOR_ADVICE[evidence.detector]}
          </p>
        )}
        <Button asChild variant="outline" size="sm">
          <Link to={`/projects/${projectSlug}/traces/${evidence.traceId}`}>
            Open the trace
          </Link>
        </Button>
      </CardContent>
    </Card>
  )
}
