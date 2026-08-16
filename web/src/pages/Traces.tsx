import { Routes, Route } from 'react-router'
import { ProjectResponse } from '@/api/client'
import TracesList from './TracesList'
import TraceDetail from './TraceDetail'
import TraceOperations from './TraceOperations'

interface TracesProps {
  project: ProjectResponse
}

export default function Traces({ project }: TracesProps) {
  return (
    <Routes>
      <Route index element={<TracesList project={project} />} />
      {/* Static segment registered before `:traceId` so "operations" is not
          matched as a trace id. */}
      <Route path="operations" element={<TraceOperations project={project} />} />
      <Route path=":traceId" element={<TraceDetail project={project} />} />
    </Routes>
  )
}
