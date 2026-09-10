// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useRoute } from "./router";
import { AuditList } from "./components/AuditList";
import { AuditDetail } from "./components/AuditDetail";
import { HistoryPage } from "./components/HistoryPage";
import { Settings } from "./components/Settings";

export function App() {
  const route = useRoute();

  switch (route.kind) {
    case "list":
      return <AuditList />;
    case "audit":
      return <AuditDetail auditId={route.auditId} />;
    case "history":
      return <HistoryPage />;
    case "settings":
      return <Settings />;
  }
}
