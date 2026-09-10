// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

interface StatCardProps {
  label: string;
  value: string | number;
  color?: string;
}

export function StatCard({ label, value, color }: StatCardProps) {
  return (
    <div className="stat-card">
      <div className="label">{label}</div>
      <div className="value" style={color ? { color } : undefined}>
        {value}
      </div>
    </div>
  );
}
