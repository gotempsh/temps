// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/// <reference types="@rsbuild/core/types" />

interface ImportMetaEnv {
  readonly TEMPS_VERSION: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}
