// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import React, { useEffect, useState } from "react";

interface ProjectInfo {
  id: number;
  name: string;
  preset: string;
  lastDeployment?: string;
}

export function App() {
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [greeting, setGreeting] = useState<string>("");
  const [error, setError] = useState<string>("");

  useEffect(() => {
    fetch("/x/hello-python/hello")
      .then((r) => r.json())
      .then((data) => setGreeting(data.message))
      .catch((e) => setError(e.message));

    fetch("/x/hello-python/projects")
      .then((r) => r.json())
      .then((data) => setProjects(data.projects ?? []))
      .catch((e) => setError(e.message));
  }, []);

  return (
    <div style={{ fontFamily: "system-ui", padding: "2rem", maxWidth: 800 }}>
      <h1>Hello Python Plugin</h1>
      {error && <p style={{ color: "red" }}>Error: {error}</p>}
      {greeting && <p style={{ fontSize: "1.25rem" }}>{greeting}</p>}

      <h2>Projects</h2>
      {projects.length === 0 ? (
        <p>No projects found</p>
      ) : (
        <table style={{ width: "100%", borderCollapse: "collapse" }}>
          <thead>
            <tr>
              <th style={thStyle}>ID</th>
              <th style={thStyle}>Name</th>
              <th style={thStyle}>Preset</th>
              <th style={thStyle}>Last Deploy</th>
            </tr>
          </thead>
          <tbody>
            {projects.map((p) => (
              <tr key={p.id}>
                <td style={tdStyle}>{p.id}</td>
                <td style={tdStyle}>{p.name}</td>
                <td style={tdStyle}>{p.preset}</td>
                <td style={tdStyle}>{p.lastDeployment ?? "never"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

const thStyle: React.CSSProperties = {
  textAlign: "left",
  padding: "0.5rem",
  borderBottom: "2px solid #ddd",
};

const tdStyle: React.CSSProperties = {
  padding: "0.5rem",
  borderBottom: "1px solid #eee",
};
