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
      {/* The console alone, no sandbox layout or intro: what a user of the product would see. */}
      <Route path="/console" element={<ConsoleV1Page full />} />
      <Route path="/landing" element={<InkLandingV1Page full />} />
      <Route path="/status" element={<StatusPage full />} />
      {/* The consolidated guide: chrome-free, the reading entry point for the whole system. */}
      <Route path="/guide" element={<GuidePage />} />
      <Route path="*" element={
    <Layout>
      <Routes>
        <Route path="/" element={<Navigate to="/brand" replace />} />
        <Route path="/brand" element={<BrandPage />} />
        <Route path="/v1" element={<ConsoleV1Page />} />
        <Route path="/v1-landing" element={<InkLandingV1Page />} />
        <Route path="/status-page" element={<StatusPage />} />
        <Route path="/op-components" element={<OpComponentsPage />} />
        <Route path="/agent" element={<AgentChatPage />} />
        <Route path="/foundations" element={<FoundationsPage />} />
        <Route path="/components" element={<ComponentsPage />} />
        <Route path="/patterns" element={<PatternsPage />} />
        <Route path="/kitchen-sink" element={<KitchenSinkPage />} />
      </Routes>
    </Layout>
      } />
    </Routes>
  )
}
