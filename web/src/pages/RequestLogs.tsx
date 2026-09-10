// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Routes, Route } from 'react-router'
import { ProjectResponse } from '@/api/client'
import RequestLogsList from './RequestLogsList'
import RequestLogDetail from './RequestLogDetail'

interface RequestLogsProps {
  project: ProjectResponse
}

export default function RequestLogs({
  project: projectResponse,
}: RequestLogsProps) {
  return (
    <Routes>
      <Route index element={<RequestLogsList project={projectResponse} />} />
      <Route
        path=":logId"
        element={<RequestLogDetail project={projectResponse} />}
      />
    </Routes>
  )
}
