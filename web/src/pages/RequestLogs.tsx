import { Routes, Route } from 'react-router-dom'
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
