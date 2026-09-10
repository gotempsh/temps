// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { scoreClass } from "../utils";

interface ScoreProps {
  value: number;
  large?: boolean;
}

export function Score({ value, large }: ScoreProps) {
  return (
    <span className={`score ${scoreClass(value)} ${large ? "score-lg" : ""}`}>
      {value}
    </span>
  );
}
