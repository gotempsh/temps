// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import ProxyLogsList from '@/components/logs/ProxyLogsList'
import type { ListReturnState } from '@/components/table/list-navigation'

interface RequestLogsListProps {
  project: ProjectResponse
  listState: ListReturnState | null
  onRememberList: (state: ListReturnState) => void
}

export default function RequestLogsList({
  project: projectResponse,
  listState,
  onRememberList,
}: RequestLogsListProps) {
  return (
    <ProxyLogsList
      project={projectResponse}
      listState={listState}
      onRememberList={onRememberList}
    />
  )
}
