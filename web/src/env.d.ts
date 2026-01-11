/// <reference types="@rsbuild/core/types" />

interface ImportMetaEnv {
  readonly TEMPS_VERSION: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}
