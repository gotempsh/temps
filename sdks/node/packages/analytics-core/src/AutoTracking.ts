// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Events the tracker emits without any code from the site owner beyond
 * installing it (ADR 109 D1): clicks on links to other sites, clicks on file
 * downloads, and pages the site marks as "not found".
 *
 * They are ordinary events. They show up in the event list and event detail
 * pages like any `trackEvent()` call, and can be used as funnel steps and
 * conversion goals with no extra work.
 */

import type { JsonValue } from "./types";

/** Event emitted when a visitor clicks a link to another host. */
export const OUTBOUND_CLICK_EVENT = "outbound_click";
/** Event emitted when a visitor clicks a link to a downloadable file. */
export const FILE_DOWNLOAD_EVENT = "file_download";
/** Event emitted for a page the site reports as missing. */
export const NOT_FOUND_EVENT = "not_found";
/**
 * Attribute that marks a rendered page as a "not found" page. Put it on any
 * element of your 404 page, e.g. `<main data-temps-404>`.
 */
export const NOT_FOUND_MARKER_ATTRIBUTE = "data-temps-404";

/**
 * Extensions counted as file downloads when `fileDownloadExtensions` is not
 * set: documents, spreadsheets and slides, archives, audio and video, disk
 * images and installers. Spread it to extend rather than replace:
 * `fileDownloadExtensions: [...DEFAULT_FILE_DOWNLOAD_EXTENSIONS, "stl"]`.
 */
export const DEFAULT_FILE_DOWNLOAD_EXTENSIONS: readonly string[] = [
  // Documents
  "pdf", "doc", "docx", "odt", "rtf", "txt", "md", "epub",
  // Spreadsheets and slides
  "xls", "xlsx", "ods", "csv", "ppt", "pptx", "pps", "odp", "key",
  // Archives
  "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz",
  // Audio and video
  "mp3", "wav", "wma", "flac", "midi", "mp4", "mov", "avi", "wmv", "mpeg", "webm", "mkv",
  // Disk images and installers
  "dmg", "iso", "exe", "msi", "pkg", "deb", "rpm", "apk", "appimage",
];

/**
 * How long after a page view the tracker looks for the 404 marker a second
 * time. Single-page apps render their "not found" view after the route change
 * that produced the page view, so the first check usually runs too early.
 */
export const NOT_FOUND_RECHECK_DELAY_MS = 1000;

export interface AutoTrackOptions {
  /**
   * Emit `outbound_click` when a visitor clicks a link to another host.
   * Defaults to true.
   */
  autoTrackOutboundLinks?: boolean;
  /**
   * Emit `file_download` when a visitor clicks a link whose path ends in one
   * of `fileDownloadExtensions`, or that carries the `download` attribute.
   * Defaults to true.
   */
  autoTrackFileDownloads?: boolean;
  /**
   * File extensions (without the dot, case-insensitive) counted as downloads.
   * Replaces the default list; defaults to `DEFAULT_FILE_DOWNLOAD_EXTENSIONS`.
   */
  fileDownloadExtensions?: readonly string[];
  /**
   * Emit `not_found` after a page view when the page contains an element with
   * the `data-temps-404` attribute. Defaults to true; pages without the marker
   * are unaffected. `trackNotFound()` works either way.
   */
  autoTrackNotFound?: boolean;
}

/** What a link click resolves to, or `null` when it is not tracked. */
export interface LinkClassification {
  eventName: typeof OUTBOUND_CLICK_EVENT | typeof FILE_DOWNLOAD_EVENT;
  data: Record<string, JsonValue>;
}

/** The parts of a link the classifier needs; an `HTMLAnchorElement` fits. */
export interface LinkLike {
  href: string;
  hasAttribute(name: string): boolean;
}

export interface ClassifyLinkOptions {
  trackOutboundLinks: boolean;
  trackFileDownloads: boolean;
  fileDownloadExtensions: readonly string[];
}

/**
 * The recorded URL: origin and path only. Query strings and fragments are
 * dropped on purpose. Download links are often pre-signed object-store URLs
 * whose query string is a credential, and outbound query strings mostly carry
 * tracking parameters that would split one destination into thousands of rows.
 */
function recordedUrl(url: URL): string {
  return `${url.origin}${url.pathname}`;
}

/** Lower-cased extension of the last path segment, or `null` when it has none. */
export function pathExtension(pathname: string): string | null {
  const segment = pathname.slice(pathname.lastIndexOf("/") + 1);
  const dot = segment.lastIndexOf(".");
  if (dot <= 0 || dot === segment.length - 1) return null;
  return segment.slice(dot + 1).toLowerCase();
}

/**
 * Decide whether a click on `link` is a file download, an outbound click, or
 * neither. A download takes precedence: a PDF on a CDN is reported once, as a
 * download, rather than twice. Only `http:` and `https:` links are tracked;
 * `mailto:`, `tel:`, `javascript:` and `blob:` links are ignored.
 */
export function classifyLink(
  link: LinkLike,
  currentHost: string,
  options: ClassifyLinkOptions
): LinkClassification | null {
  let url: URL;
  try {
    url = new URL(link.href);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;

  if (options.trackFileDownloads) {
    const extension = pathExtension(url.pathname);
    const listed =
      extension !== null &&
      options.fileDownloadExtensions.some((candidate) => candidate.toLowerCase() === extension);
    if (listed || link.hasAttribute("download")) {
      return {
        eventName: FILE_DOWNLOAD_EVENT,
        data: { url: recordedUrl(url), extension: extension ?? "", host: url.host },
      };
    }
  }

  if (options.trackOutboundLinks && url.host !== currentHost) {
    return {
      eventName: OUTBOUND_CLICK_EVENT,
      data: { url: recordedUrl(url), host: url.host },
    };
  }

  return null;
}

/**
 * Install one delegated listener on the document for outbound links and file
 * downloads. No per-link handlers: links added later are covered too. The
 * same handler also receives `auxclick`, because a middle click opens the
 * link in a new tab without firing `click`.
 *
 * `send` must survive the navigation the click starts, so callers pass a
 * beacon-based sender (`sendAnalyticsReliable`).
 */
export function setupLinkTracking(
  options: AutoTrackOptions,
  send: (eventName: string, data: Record<string, JsonValue>) => void
): () => void {
  const classify: ClassifyLinkOptions = {
    trackOutboundLinks: options.autoTrackOutboundLinks ?? true,
    trackFileDownloads: options.autoTrackFileDownloads ?? true,
    fileDownloadExtensions: options.fileDownloadExtensions ?? DEFAULT_FILE_DOWNLOAD_EXTENSIONS,
  };
  if (!classify.trackOutboundLinks && !classify.trackFileDownloads) {
    return (): void => {};
  }

  const onClick = (event: MouseEvent): void => {
    // Primary click, or a middle click (auxclick). Right clicks open a menu.
    if (event.type === "auxclick" ? event.button !== 1 : event.button !== 0) return;
    const target = event.target as Element | null;
    const link = target?.closest?.("a[href]");
    if (!link || !("href" in link)) return;
    const result = classifyLink(link as HTMLAnchorElement, window.location.host, classify);
    if (result) send(result.eventName, result.data);
  };

  document.addEventListener("click", onClick);
  document.addEventListener("auxclick", onClick);
  return (): void => {
    document.removeEventListener("click", onClick);
    document.removeEventListener("auxclick", onClick);
  };
}

/** Properties of a `not_found` event. `path` is the page the visitor asked for. */
export function notFoundEventData(extra: Record<string, JsonValue> = {}): Record<string, JsonValue> {
  // Outside a browser there is no page to describe.
  if (typeof window === "undefined" || typeof document === "undefined") return { ...extra };
  return {
    path: window.location.pathname,
    referrer: document.referrer,
    ...extra,
  };
}

/** Whether the current document carries the `data-temps-404` marker. */
export function hasNotFoundMarker(): boolean {
  return document.querySelector(`[${NOT_FOUND_MARKER_ATTRIBUTE}]`) !== null;
}

/**
 * Watches for the 404 marker after each page view. Call `afterPageview()`
 * whenever a page view is recorded; it emits `not_found` at most once for
 * that page view, checking immediately and once more after
 * `NOT_FOUND_RECHECK_DELAY_MS` for apps that render the 404 view late.
 *
 * On an in-app navigation the page view is recorded when the URL changes,
 * before the router has replaced the old view, so a marker already in the
 * document at that moment belongs to the page being left and is ignored. Only
 * the first page view of a document counts a marker that is already there.
 */
export class NotFoundMarkerWatcher {
  private timer: ReturnType<typeof setTimeout> | null = null;
  private reported = false;
  /**
   * Markers seen while the previous page view was current: a view the router
   * has not torn down yet when the next page view is recorded. A marker the
   * new route rendered before its page view (a manual `trackPageview()` in
   * an effect) was never seen, so it still counts.
   */
  private previousMarkers: Set<Element> = new Set();
  private observedMarkers: Set<Element> = new Set();
  private path = "";

  constructor(private readonly report: () => void) {}

  afterPageview(): void {
    this.cancel();
    this.reported = false;
    this.path = window.location.pathname;
    this.previousMarkers = this.observedMarkers;
    this.observedMarkers = new Set();
    if (this.check()) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.check();
    }, NOT_FOUND_RECHECK_DELAY_MS);
  }

  /** Stop any pending re-check. */
  cancel(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }

  private check(): boolean {
    if (this.reported) return false;
    // The visitor moved on; that navigation records its own page view.
    if (window.location.pathname !== this.path) return false;
    const markers = markerElements();
    for (const element of markers) this.observedMarkers.add(element);
    const fresh = markers.some((element) => !this.previousMarkers.has(element));
    if (!fresh) return false;
    this.reported = true;
    this.report();
    return true;
  }
}

function markerElements(): Element[] {
  return Array.from(document.querySelectorAll(`[${NOT_FOUND_MARKER_ATTRIBUTE}]`));
}
