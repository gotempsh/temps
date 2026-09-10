// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useRoute, submissionsPath, settingsPath } from "./router";
import Submissions from "./components/Submissions";
import Settings from "./components/Settings";

export default function App() {
  const route = useRoute();

  return (
    <div className="app">
      <nav className="tab-bar">
        <a
          href={submissionsPath()}
          className={`tab ${route.kind === "submissions" ? "active" : ""}`}
        >
          Submissions
        </a>
        <a
          href={settingsPath()}
          className={`tab ${route.kind === "settings" ? "active" : ""}`}
        >
          Settings
        </a>
      </nav>

      <main className="content">
        {route.kind === "submissions" && <Submissions />}
        {route.kind === "settings" && <Settings />}
      </main>
    </div>
  );
}
