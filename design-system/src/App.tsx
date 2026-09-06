// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Navigate, Route, Routes } from 'react-router'
import { Layout } from '@/components/Layout'
import { StatusPage } from '@/sections/StatusPage'
import { BrandPage } from '@/sections/Brand'
import { ComponentsPage } from '@/sections/Components'
import { FoundationsPage } from '@/sections/Foundations'
import { GuidePage } from '@/sections/Guide'
import { KitchenSinkPage } from '@/sections/KitchenSink'
import { ConsoleV1Page } from '@/sections/ConsoleV1'
import { InkLandingV1Page } from '@/sections/InkLandingV1'
import { OpComponentsPage } from '@/sections/OpComponents'
import { AgentChatPage } from '@/sections/AgentChat'
import { PatternsPage } from '@/sections/Patterns'

export default function App() {
  return (
    <Routes>
      {/* The product with no sandbox around it: what a user would actually see. */}
      <Route path="/console" element={<ConsoleV1Page full />} />
      <Route path="/landing" element={<InkLandingV1Page full />} />
      <Route path="/status" element={<StatusPage full />} />
      {/* Everything else is documentation, and documentation has one chrome. */}
      <Route path="*" element={
        <Layout>
          <Routes>
            <Route path="/" element={<Navigate to="/guide" replace />} />
            <Route path="/guide" element={<GuidePage />} />
            <Route path="/brand" element={<BrandPage />} />
            <Route path="/foundations" element={<FoundationsPage />} />
            <Route path="/components" element={<ComponentsPage />} />
            <Route path="/op-components" element={<OpComponentsPage />} />
            <Route path="/patterns" element={<PatternsPage />} />
            <Route path="/kitchen-sink" element={<KitchenSinkPage />} />
            <Route path="/v1" element={<ConsoleV1Page />} />
            <Route path="/v1-landing" element={<InkLandingV1Page />} />
            <Route path="/status-page" element={<StatusPage />} />
            <Route path="/agent" element={<AgentChatPage />} />
          </Routes>
        </Layout>
      } />
    </Routes>
  )
}
