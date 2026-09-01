// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export { FlagsClient, flags } from './client';
export { evaluate, valueMatchesType } from './evaluate';
export type {
  EvalContext,
  EvalReason,
  Evaluation,
  FlagSnapshot,
  FlagSnapshotResponse,
  FlagValueType,
  FlagsClientOptions,
} from './types';
