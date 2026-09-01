// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useRoute, submissionsPath, suggestionsPath, settingsPath } from "./router";
import { Submissions } from "./components/Submissions";
import { Suggestions } from "./components/Suggestions";
import { Settings } from "./components/Settings";

export function App() {
  const route = useRoute();

  return (
    <div>
      <nav className="tab-bar">
        <a
          href={submissionsPath()}
          className={`tab ${route.kind === "submissions" ? "tab-active" : ""}`}
        >
          Submissions
        </a>
        <a
          href={suggestionsPath()}
          className={`tab ${route.kind === "suggestions" ? "tab-active" : ""}`}
        >
          Suggestions
        </a>
        <a
          href={settingsPath()}
          className={`tab ${route.kind === "settings" ? "tab-active" : ""}`}
        >
          Settings
        </a>
      </nav>

      <div className="content">
        {route.kind === "submissions" && <Submissions />}
        {route.kind === "suggestions" && <Suggestions />}
        {route.kind === "settings" && <Settings />}
      </div>
    </div>
  );
}
