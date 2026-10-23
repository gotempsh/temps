// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { client } from '@/api/client/client.gen'

const BEARER_SECURITY = [{ scheme: 'bearer', type: 'http' }] as const

/** Where the server's `setup_path` points (`temps_functions::SETUP_PATH`). */
export const FUNCTIONS_SETUP_PATH = '/settings/functions'

/** Largest WASM module the server accepts (`MAX_WASM_BYTES`). */
export const MAX_WASM_BYTES = 32 * 1024 * 1024

/** Longest function name the server accepts (`MAX_NAME_LEN`). */
export const MAX_FUNCTION_NAME_LENGTH = 40

/** `asleep`, `waking`, `awake`, `sleeping`, or `unregistered`. */
export type FunctionState =
  'asleep' | 'waking' | 'awake' | 'sleeping' | 'unregistered'

/** Mirrors `FunctionResponse`. */
export interface FunctionRecord {
  id: number
  project_id: number
  name: string
  /** `wasm` or `oci`. */
  artifact_kind: string
  image_reference: string | null
  wasm_digest: string | null
  wasm_size_bytes: number | null
  port: number
  idle_timeout_seconds: number
  wake_timeout_seconds: number
  memory_limit_mb: number | null
  /** Requests to this hostname reach the function. */
  hostname: string
  workload_id: number | null
  state: string
  ready_reason: string | null
  ready_message: string | null
  created_at: string
  updated_at: string
}

/** Mirrors `FunctionListResponse`. */
export interface FunctionList {
  functions: FunctionRecord[]
  total: number
  page: number
  page_size: number
  total_pages: number
}

/** Mirrors `RuntimeAvailabilityResponse`. */
export interface RuntimeAvailability {
  available: boolean
  reason: string | null
}

/** Mirrors `FunctionsCapabilityResponse`. */
export interface FunctionsCapability {
  configured: boolean
  wasm: RuntimeAvailability
  oci: RuntimeAvailability
  reason: string | null
  setup_path: string | null
}

/** Mirrors `SleepFunctionResponse`. */
export interface SleepFunctionResult {
  function: FunctionRecord
  /** Whether it was running and is now stopped. */
  slept: boolean
}

/** Mirrors `CreateFunctionRequest`: exactly one of `wasm_base64` and `image`. */
export interface CreateFunctionRequest {
  name: string
  wasm_base64?: string
  image?: string
  port?: number
  idle_timeout_seconds?: number
  wake_timeout_seconds?: number
  memory_limit_mb?: number
}

export const functionsKeys = {
  capability: ['functions', 'capability'] as const,
  list: (projectId: number, page: number, pageSize: number) =>
    ['functions', 'list', projectId, page, pageSize] as const,
  project: (projectId: number) => ['functions', 'list', projectId] as const,
  detail: (projectId: number, functionId: number) =>
    ['functions', 'detail', projectId, functionId] as const,
}

export async function getFunctionsCapability(): Promise<FunctionsCapability> {
  const { data } = await client.get<FunctionsCapability, unknown, true>({
    security: [...BEARER_SECURITY],
    url: '/functions/capability',
    throwOnError: true,
  })
  return data
}

export async function listFunctions(
  projectId: number,
  page: number,
  pageSize: number
): Promise<FunctionList> {
  const { data } = await client.get<FunctionList, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `/projects/${projectId}/functions`,
    query: { page, page_size: pageSize },
    throwOnError: true,
  })
  return data
}

export async function getFunction(
  projectId: number,
  functionId: number
): Promise<FunctionRecord> {
  const { data } = await client.get<FunctionRecord, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `/projects/${projectId}/functions/${functionId}`,
    throwOnError: true,
  })
  return data
}

export async function createFunction(
  projectId: number,
  request: CreateFunctionRequest
): Promise<FunctionRecord> {
  const { data } = await client.post<FunctionRecord, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `/projects/${projectId}/functions`,
    body: request,
    headers: { 'Content-Type': 'application/json' },
    throwOnError: true,
  })
  return data
}

export async function deleteFunction(
  projectId: number,
  functionId: number
): Promise<void> {
  await client.delete<unknown, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `/projects/${projectId}/functions/${functionId}`,
    throwOnError: true,
  })
}

export async function wakeFunction(
  projectId: number,
  functionId: number
): Promise<FunctionRecord> {
  const { data } = await client.post<FunctionRecord, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `/projects/${projectId}/functions/${functionId}/wake`,
    throwOnError: true,
  })
  return data
}

export async function sleepFunction(
  projectId: number,
  functionId: number
): Promise<SleepFunctionResult> {
  const { data } = await client.post<SleepFunctionResult, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `/projects/${projectId}/functions/${functionId}/sleep`,
    throwOnError: true,
  })
  return data
}

// ── Pure helpers ───────────────────────────────────────────────────────────

/** The function's URL, using the console's own scheme. */
export function functionUrl(hostname: string, protocol = 'https:'): string {
  return `${protocol}//${hostname}`
}

/** Badge text and tone for a function's state. */
export function functionStateBadge(state: string): {
  label: string
  tone: 'success' | 'pending' | 'neutral' | 'warning'
} {
  switch (state) {
    case 'awake':
      return { label: 'Awake', tone: 'success' }
    case 'waking':
      return { label: 'Waking', tone: 'pending' }
    case 'sleeping':
      return { label: 'Going to sleep', tone: 'pending' }
    case 'asleep':
      return { label: 'Asleep', tone: 'neutral' }
    case 'unregistered':
      return { label: 'Not registered', tone: 'warning' }
    default:
      return { label: state, tone: 'neutral' }
  }
}

/** Whether Wake / Sleep make sense for a state. */
export function canWake(state: string): boolean {
  return state === 'asleep' || state === 'sleeping'
}
export function canSleep(state: string): boolean {
  return state === 'awake' || state === 'waking'
}

/** Mirrors the server's name rule: lowercase, digits, single inner hyphens. */
export function functionNameError(name: string): string | null {
  if (name === '') return 'Enter a name, e.g. resize-image.'
  if (name.length > MAX_FUNCTION_NAME_LENGTH) {
    return `Use at most ${MAX_FUNCTION_NAME_LENGTH} characters.`
  }
  if (!/^[a-z][a-z0-9]*(-[a-z0-9]+)*$/.test(name)) {
    return 'Start with a lowercase letter and use only lowercase letters, digits and single inner hyphens, e.g. resize-image.'
  }
  return null
}

/** WASM modules start with `\0asm` and version 1 (WASI preview 1 core modules). */
export function wasmModuleError(bytes: Uint8Array): string | null {
  if (bytes.length === 0) return 'The file is empty.'
  if (bytes.length > MAX_WASM_BYTES) {
    return `The module is ${bytes.length} bytes; the limit is ${MAX_WASM_BYTES}.`
  }
  const magic = [0x00, 0x61, 0x73, 0x6d]
  if (bytes.length < 8 || magic.some((b, i) => bytes[i] !== b)) {
    return 'The file is not a WASM binary (it does not start with \\0asm).'
  }
  if (bytes[4] !== 1 || bytes[5] !== 0 || bytes[6] !== 0 || bytes[7] !== 0) {
    return 'The file is a WASM component or an unknown WASM version; functions run WASI preview 1 core modules (compile for wasm32-wasip1).'
  }
  return null
}

/** The raw create-form values. */
export interface FunctionFormValues {
  name: string
  artifact: 'wasm' | 'oci'
  image: string
  port: string
  idle_timeout_seconds: string
  wake_timeout_seconds: string
  memory_limit_mb: string
}

type FormResult<T> =
  | { ok: true; value: T }
  | { ok: false; error: string; field: keyof FunctionFormValues | 'wasm' }

function optionalInt(
  text: string,
  field: keyof FunctionFormValues,
  label: string,
  min: number,
  max: number
): FormResult<number | undefined> {
  const trimmed = text.trim()
  if (trimmed === '') return { ok: true, value: undefined }
  const value = Number(trimmed)
  if (!Number.isInteger(value) || value < min || value > max) {
    return {
      ok: false,
      error: `${label} must be a whole number between ${min} and ${max}.`,
      field,
    }
  }
  return { ok: true, value }
}

/**
 * Build the create request from the form and an already-encoded module
 * (`wasmBase64`, for a WASM function). Returns the sentence explaining what
 * is wrong instead when the form cannot be sent.
 */
export function buildCreateFunctionRequest(
  form: FunctionFormValues,
  wasmBase64: string | null
): FormResult<CreateFunctionRequest> {
  const name = form.name.trim()
  const nameError = functionNameError(name)
  if (nameError) return { ok: false, error: nameError, field: 'name' }
  const value: CreateFunctionRequest = { name }
  if (form.artifact === 'wasm') {
    if (!wasmBase64) {
      return {
        ok: false,
        error: 'Choose a .wasm module to upload.',
        field: 'wasm',
      }
    }
    value.wasm_base64 = wasmBase64
  } else {
    const image = form.image.trim()
    if (image === '') {
      return {
        ok: false,
        error: 'Enter an image reference, e.g. registry.example.com/team/fn:1.',
        field: 'image',
      }
    }
    value.image = image
  }
  const port = optionalInt(form.port, 'port', 'Port', 1, 65535)
  if (!port.ok) return port
  const idle = optionalInt(
    form.idle_timeout_seconds,
    'idle_timeout_seconds',
    'Idle timeout',
    10,
    86400
  )
  if (!idle.ok) return idle
  const wake = optionalInt(
    form.wake_timeout_seconds,
    'wake_timeout_seconds',
    'Wake timeout',
    1,
    300
  )
  if (!wake.ok) return wake
  const memory = optionalInt(
    form.memory_limit_mb,
    'memory_limit_mb',
    'Memory limit',
    1,
    4096
  )
  if (!memory.ok) return memory
  if (port.value !== undefined) value.port = port.value
  if (idle.value !== undefined) value.idle_timeout_seconds = idle.value
  if (wake.value !== undefined) value.wake_timeout_seconds = wake.value
  if (memory.value !== undefined) value.memory_limit_mb = memory.value
  return { ok: true, value }
}

/** Base64 of a module's bytes, in chunks so a 32 MB module does not overflow the stack. */
export function bytesToBase64(bytes: Uint8Array): string {
  let binary = ''
  const chunk = 0x8000
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk))
  }
  return btoa(binary)
}

/** Which artifact kinds can be created right now, from the capability. */
export function availableArtifacts(
  capability: FunctionsCapability
): Array<'wasm' | 'oci'> {
  const kinds: Array<'wasm' | 'oci'> = []
  if (capability.wasm.available) kinds.push('wasm')
  if (capability.oci.available) kinds.push('oci')
  return kinds
}
