// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createApiKeyMutation } from '@/api/client/@tanstack/react-query.gen'
import { useSettingsPage, useUpdateSettings } from '@/hooks/useSettings'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { invalidateApiKeyQueries } from '@/lib/api-key-queries'
import { MCP_KEY_ROLES, type McpKeyAccess, mcpKeyName } from '@/lib/mcp-clients'
import { sensitiveActionErrorMessage } from '@/lib/sensitiveActionProblem'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import type { McpServerState } from './agents'

/**
 * What every MCP app needs before it can connect: the MCP server on, and a
 * key of its own. `client` names the key (`mcp-<client>-<time>`).
 */
export function useMcpAccess(client: string) {
  const queryClient = useQueryClient()
  const settings = useSettingsPage()
  const turnOn = useUpdateSettings()
  const [access, setAccess] = useState<McpKeyAccess>('changes')
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const createKey = useMutation({
    ...createApiKeyMutation(),
    meta: { errorTitle: 'Failed to create the key' },
    onSuccess: async () => {
      await invalidateApiKeyQueries(queryClient)
      toast.success('Key created')
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => createKey.mutate(variables)))
        return
      toast.error(
        sensitiveActionErrorMessage(error, 'Failed to create the key')
      )
    },
  })

  const mcpState: McpServerState = settings.error
    ? 'unknown'
    : !settings.data
      ? 'loading'
      : settings.data.mcp_server?.enabled === true
        ? 'on'
        : 'off'

  return {
    mcpState,
    turnOn,
    access,
    setAccess,
    createKey,
    create: () =>
      createKey.mutate({
        body: {
          name: mcpKeyName(client, new Date()),
          role_type: MCP_KEY_ROLES[access],
          expires_at: null,
          permissions: null,
        },
      }),
    apiKey: createKey.data?.api_key ?? null,
    error: createKey.error ?? turnOn.error,
    verificationDialog,
  }
}

export type McpAccess = ReturnType<typeof useMcpAccess>
