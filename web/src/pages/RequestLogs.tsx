// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Routes, Route } from 'react-router'
import { ProjectResponse } from '@/api/client'
import { useListReturnNavigation } from '@/components/table/list-navigation'
import RequestLogsList from './RequestLogsList'
import RequestLogDetail from './RequestLogDetail'

interface RequestLogsProps {
  project: ProjectResponse
}

export default function RequestLogs({
  project: projectResponse,
}: RequestLogsProps) {
  const { listState, setListState, containerRef } = useListReturnNavigation(
    `/projects/${projectResponse.slug}/request-logs`
  )

  return (
    <div ref={containerRef} className="w-full min-w-0">
      <Routes>
        <Route
          index
          element={
            <RequestLogsList
              project={projectResponse}
              listState={listState}
              onRememberList={setListState}
            />
          }
        />
        <Route
          path=":logId"
          element={
            <RequestLogDetail project={projectResponse} listState={listState} />
          }
        />
      </Routes>
    </div>
  )
}
