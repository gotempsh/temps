// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

const PRESET_ICON_PATHS: Record<string, string> = {
  angular: '/presets/angular.svg',
  astro: '/presets/astro.svg',
  autopack: '/presets/autopack.svg',
  custom: '/presets/custom.svg',
  dart: '/presets/dart.svg',
  deno: '/presets/deno.svg',
  django: '/presets/django.svg',
  docker: '/presets/docker.svg',
  dockercompose: '/presets/docker.svg',
  dockerfile: '/presets/dockerfile.svg',
  docusaurus: '/presets/docusaurus.svg',
  dotnet: '/presets/dotnet.svg',
  elixir: '/presets/elixir.svg',
  fastapi: '/presets/fastapi.svg',
  flask: '/presets/flask.svg',
  go: '/presets/go.svg',
  java: '/presets/java.svg',
  laravel: '/presets/laravel.svg',
  nextjs: '/presets/nextjs.svg',
  nixpacks: '/presets/nixpacks.svg',
  nixpacksnode: '/presets/nodejs.svg',
  nixpacksstatic: '/presets/static.svg',
  node: '/presets/nodejs.svg',
  nodejs: '/presets/nodejs.svg',
  nuxt: '/presets/nuxt.svg',
  php: '/presets/php.svg',
  python: '/presets/python.svg',
  rails: '/presets/rails.svg',
  react: '/presets/react.svg',
  remix: '/presets/remix.svg',
  rsbuild: '/presets/rsbuild.svg',
  ruby: '/presets/ruby.svg',
  rust: '/presets/rust.svg',
  solidstart: '/presets/solidstart.svg',
  static: '/presets/static.svg',
  sveltekit: '/presets/sveltekit.svg',
  vite: '/presets/vite.svg',
  vue: '/presets/vue.svg',
}

const DARK_MODE_INVERT_PRESETS = new Set(['deno', 'nextjs', 'remix', 'rust'])

function normalizedPreset(preset: string): string {
  return preset.toLowerCase().replace(/[^a-z0-9]/g, '')
}

export function presetIconPath(preset: string): string | undefined {
  return PRESET_ICON_PATHS[normalizedPreset(preset)]
}

export function presetIconNeedsDarkInvert(preset: string): boolean {
  return DARK_MODE_INVERT_PRESETS.has(normalizedPreset(preset))
}
