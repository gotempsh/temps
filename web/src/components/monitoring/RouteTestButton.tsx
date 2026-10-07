// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { testNotificationRouteMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { useMutation } from '@tanstack/react-query'
import { Loader2, Send } from 'lucide-react'
import { toast } from 'sonner'
import { summarizeRouteTest } from './route-test-summary'

interface RouteTestButtonProps {
  routeId: number
  routeName: string
  /** Icon-only, for dense rows such as the routes list. */
  iconOnly?: boolean
}

/** Sends a sample notification through one route and reports per provider. */
export function RouteTestButton({
  routeId,
  routeName,
  iconOnly = false,
}: RouteTestButtonProps) {
  const testMutation = useMutation({
    ...testNotificationRouteMutation(),
    meta: { errorTitle: `Failed to test route "${routeName}"` },
    onSuccess: (result) => {
      const summary = summarizeRouteTest(result)
      const show = summary.tone === 'success' ? toast.success : toast.error
      show(summary.title, { description: summary.description })
    },
  })
  const label = `Send a test notification through ${routeName}`
  const icon = testMutation.isPending ? (
    <Loader2 className="h-4 w-4 animate-spin" />
  ) : (
    <Send className="h-4 w-4" />
  )

  return (
    <Button
      type="button"
      variant={iconOnly ? 'ghost' : 'outline'}
      size={iconOnly ? 'icon' : 'default'}
      aria-label={label}
      title={label}
      disabled={testMutation.isPending}
      onClick={() => testMutation.mutate({ path: { id: routeId } })}
    >
      {icon}
      {!iconOnly && <span className="ml-2">Test route</span>}
    </Button>
  )
}
