// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Who a visitor is, once `identify()` named them (ADR 109 D4): the app's
 * user id, whether the backend vouched for it or the browser claimed it, and
 * a way to every other device the same user was seen on.
 */

import type { ProjectResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { identitySourceLabel, sameUserHref } from '@/lib/visitor-identity'
import { format } from 'date-fns'
import { Link } from 'react-router'

export interface VisitorIdentityFields {
  user_id?: string | null
  identity_source?: string | null
  identified_at?: string | null
}

export function VisitorIdentityPanel({
  project,
  visitor,
}: {
  project: ProjectResponse
  visitor: VisitorIdentityFields
}) {
  const source = identitySourceLabel(visitor.identity_source)
  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="text-base">User</CardTitle>
        <CardDescription>
          {visitor.user_id
            ? "Your app's id for this person, set by identify()."
            : 'Anonymous: identify() has not named this visitor.'}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3 text-sm">
        {visitor.user_id ? (
          <>
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-mono break-all">{visitor.user_id}</span>
              <CopyButton value={visitor.user_id} className="h-7 w-7 p-0" />
              {source && (
                <Badge
                  variant={
                    visitor.identity_source === 'server'
                      ? 'secondary'
                      : 'outline'
                  }
                  title={source.description}
                >
                  {source.label}
                </Badge>
              )}
            </div>
            {visitor.identified_at && (
              <p className="text-xs text-muted-foreground">
                Identified {format(new Date(visitor.identified_at), 'PPp')}
              </p>
            )}
            <Link
              to={sameUserHref(project.slug, visitor.user_id)}
              className="text-sm text-primary underline-offset-4 hover:underline"
            >
              Same user on other devices
            </Link>
          </>
        ) : (
          <p className="text-muted-foreground">
            Call <code>identify(user.id)</code> at signup or sign-in to link
            visits to your users.{' '}
            <Link
              to={`/projects/${project.slug}/analytics/settings`}
              className="text-primary underline-offset-4 hover:underline"
            >
              How to identify users
            </Link>
          </p>
        )}
      </CardContent>
    </Card>
  )
}
