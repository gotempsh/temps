import { ProjectResponse } from '@/api/client/types.gen'
import * as React from 'react'
import { Routes, Route, useParams } from 'react-router-dom'
import { VisitorsList } from './VisitorsList'
import { VisitorDetail } from './VisitorDetail'
import { SessionDetail } from './SessionDetail'
import { SessionReplayDetail } from '@/pages/SessionReplayDetail'

interface VisitorAnalyticsProps {
  project: ProjectResponse
}

// Wrapper components to inject params
function VisitorDetailWrapper({ project }: { project: ProjectResponse }) {
  const { visitorId } = useParams<{ visitorId: string }>()
  if (!visitorId) return null
  return <VisitorDetail project={project} visitorId={Number(visitorId)} />
}

function SessionDetailWrapper({ project }: { project: ProjectResponse }) {
  const { visitorId, sessionId } = useParams<{
    visitorId: string
    sessionId: string
  }>()
  if (!visitorId || !sessionId) return null
  return (
    <SessionDetail
      project={project}
      visitorId={Number(visitorId)}
      sessionId={Number(sessionId)}
    />
  )
}

export default function VisitorAnalytics({ project }: VisitorAnalyticsProps) {
  return (
    <Routes>
      <Route index element={<VisitorsList project={project} />} />
      <Route
        path=":visitorId"
        element={<VisitorDetailWrapper project={project} />}
      />
      <Route
        path=":visitorId/sessions/:sessionId"
        element={<SessionDetailWrapper project={project} />}
      />
      <Route
        path=":visitorId/session-replay/:sessionId"
        element={<SessionReplayDetail project={project} />}
      />
    </Routes>
  )
}
