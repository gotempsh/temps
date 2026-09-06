// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Project identity marks shared by every screen that names a project (list, title, palette, linked-services aside). Served by the console from the project's favicon in the real app. */
const svg = (body: string) => `data:image/svg+xml;utf8,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">${body}</svg>`)}`
export const PROJECT_ICONS: Record<string, string> = {
  'api-gateway': svg('<rect width="32" height="32" fill="#0f4c81"/><path d="M8 22 16 8l8 14z" fill="#fff"/>'),
  'acme-storefront': svg('<rect width="32" height="32" fill="#e4572e"/><circle cx="16" cy="16" r="7" fill="#fff"/>'),
  'acme-crm': svg('<rect width="32" height="32" fill="#2a9d8f"/><rect x="8" y="8" width="16" height="16" fill="#fff"/>'),
  'docs': svg('<rect width="32" height="32" fill="#f4f1de"/><path d="M9 7h11l5 5v13H9z" fill="#3d405b"/>'),
}
