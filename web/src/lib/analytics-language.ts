// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Map common language codes to human-readable names
const LANGUAGE_NAMES: Record<string, string> = {
  en: 'English',
  'en-US': 'English (US)',
  'en-GB': 'English (UK)',
  'en-AU': 'English (Australia)',
  'en-CA': 'English (Canada)',
  es: 'Spanish',
  'es-ES': 'Spanish (Spain)',
  'es-MX': 'Spanish (Mexico)',
  'es-AR': 'Spanish (Argentina)',
  fr: 'French',
  'fr-FR': 'French (France)',
  'fr-CA': 'French (Canada)',
  de: 'German',
  'de-DE': 'German (Germany)',
  'de-AT': 'German (Austria)',
  it: 'Italian',
  pt: 'Portuguese',
  'pt-BR': 'Portuguese (Brazil)',
  'pt-PT': 'Portuguese (Portugal)',
  nl: 'Dutch',
  ru: 'Russian',
  ja: 'Japanese',
  ko: 'Korean',
  zh: 'Chinese',
  'zh-CN': 'Chinese (Simplified)',
  'zh-TW': 'Chinese (Traditional)',
  ar: 'Arabic',
  hi: 'Hindi',
  tr: 'Turkish',
  pl: 'Polish',
  sv: 'Swedish',
  da: 'Danish',
  fi: 'Finnish',
  no: 'Norwegian',
  nb: 'Norwegian',
  cs: 'Czech',
  el: 'Greek',
  he: 'Hebrew',
  th: 'Thai',
  vi: 'Vietnamese',
  id: 'Indonesian',
  ms: 'Malay',
  uk: 'Ukrainian',
  ro: 'Romanian',
  hu: 'Hungarian',
  bg: 'Bulgarian',
  hr: 'Croatian',
  sk: 'Slovak',
  sl: 'Slovenian',
  lt: 'Lithuanian',
  lv: 'Latvian',
  et: 'Estonian',
  ca: 'Catalan',
  eu: 'Basque',
  gl: 'Galician',
}

export function getLanguageName(code: string): string {
  if (!code) return 'Unknown'
  // Try exact match first
  if (LANGUAGE_NAMES[code]) return LANGUAGE_NAMES[code]
  // Try base language code (e.g., "en" from "en-US")
  const base = code.split('-')[0]
  if (LANGUAGE_NAMES[base]) return `${LANGUAGE_NAMES[base]} (${code})`
  return code
}
