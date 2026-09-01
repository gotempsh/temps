// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { record, type eventWithTime } from "rrweb";
import { pack } from "@rrweb/packer";
import { SESSION_RECORDER_ENDPOINT, DEFAULT_BASE_PATH, DEFAULT_EXCLUDED_PATHS } from "./constants";
import type { SessionRecordingConfig } from "./types";

export interface SessionRecorderOptions extends SessionRecordingConfig {
  basePath?: string;
  domain?: string;
  enabled?: boolean;
  ignoreSelector?: string;
  blockSelector?: string;
  sampling?: Record<string, unknown>;
  slimDOMOptions?: Record<string, boolean>;
  maskInputOptions?: { password?: boolean; email?: boolean };
}

/**
 * A batch that has been handed to the sender. Once created it is immutable:
 * retries resend exactly these events under exactly this `batchId`, which is
 * what lets the server discard a duplicate delivery. Rebuilding the batch per
 * attempt (the previous behaviour) meant every retry carried a different set
 * of events, so no server-side dedup could ever match.
 */
interface InflightBatch {
  batchId: string;
  events: eventWithTime[];
}

/** Interaction that counts as the visitor still being present. */
const ACTIVITY_EVENTS = [
  "pointerdown",
  "keydown",
  "scroll",
  "touchstart",
  "wheel",
  "mousemove",
] as const;

/** Upper bound on how often the idle check runs, regardless of `idleTimeout`. */
const IDLE_CHECK_CEILING_MS = 15000;

function randomId(prefix: string): string {
  if (typeof crypto !== "undefined" && crypto.randomUUID) {
    return crypto.randomUUID();
  }
  return `${prefix}_${Date.now()}_${Math.random().toString(36).substring(2, 11)}`;
}

function generateSessionId(): string {
  return randomId("session");
}

function generateBatchId(): string {
  return randomId("batch");
}

function generateVisitorId(): string {
  if (typeof localStorage !== "undefined") {
    let visitorId = localStorage.getItem("temps_visitor_id");
    if (!visitorId) {
      visitorId = `visitor_${Date.now()}_${Math.random().toString(36).substring(2, 11)}`;
      localStorage.setItem("temps_visitor_id", visitorId);
    }
    return visitorId;
  }
  return `visitor_${Date.now()}_${Math.random().toString(36).substring(2, 11)}`;
}

function matchesAnyPath(currentPath: string, paths: string[]): boolean {
  return paths.some((path) => {
    const regex = new RegExp(`^${path.replace(/\*/g, ".*")}$`);
    return regex.test(currentPath);
  });
}

function getSessionMetadata(): Record<string, unknown> {
  if (typeof window === "undefined") return {};
  const screen = window.screen || ({} as Screen);
  const nav = window.navigator || ({} as Navigator);
  return {
    visitorId: generateVisitorId(),
    userAgent: nav.userAgent,
    language: nav.language,
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    screenWidth: screen.width,
    screenHeight: screen.height,
    colorDepth: screen.colorDepth || 24,
    viewportWidth: window.innerWidth,
    viewportHeight: window.innerHeight,
    url: window.location.href,
    timestamp: new Date().toISOString(),
  };
}

/**
 * Framework-agnostic rrweb wrapper. Call `start()` to begin recording and
 * `stop()` to end. `destroy()` removes all listeners and flushes pending events.
 *
 * Recording pauses while the visitor is idle or the tab is hidden, and resumes
 * on the next interaction with a fresh full snapshot. rrweb records DOM
 * mutations rather than user actions, so without that gate a page that animates
 * or mutates on a timer produces events indefinitely from a visitor who is
 * doing nothing at all.
 */
export class SessionRecorder {
  private readonly basePath: string;
  private readonly excludedPaths: string[];
  private readonly sessionSampleRate: number;
  private readonly maskAllInputs: boolean;
  private readonly maskTextSelector: string;
  private readonly blockClass: string;
  private readonly ignoreClass: string;
  private readonly maskTextClass: string;
  private readonly ignoreSelector: string;
  private readonly blockSelector: string;
  private readonly recordCanvas: boolean;
  private readonly collectFonts: boolean;
  private readonly batchSize: number;
  private readonly flushInterval: number;
  private readonly idleTimeout: number;
  private readonly pauseOnHidden: boolean;
  private readonly checkoutEveryNms: number;
  private readonly checkoutEveryNth: number;
  private readonly maxBufferedEvents: number;
  private readonly slimDOMOptions: Record<string, boolean>;
  private readonly maskInputOptions: { password?: boolean; email?: boolean };
  private readonly samplingConfig: Record<string, unknown>;

  private stopFn: (() => void) | null = null;
  private takeSnapshot: (() => void) | null = null;
  private pending: eventWithTime[] = [];
  private inflight: InflightBatch | null = null;
  private sessionId: string = "";
  private sessionInitialized: boolean = false;
  private flushTimer: ReturnType<typeof setInterval> | null = null;
  private idleTimer: ReturnType<typeof setInterval> | null = null;
  private initRetryCount: number = 0;
  private initFailed: boolean = false;
  private readonly maxInitRetries: number = 3;
  private sendRetryCount: number = 0;
  private readonly maxSendRetries: number = 5;
  private lastSendAttempt: number = 0;
  private isSending: boolean = false;
  private starting: boolean = false;
  /**
   * Bumped by every `stopRecording`. `startRecording` captures it before
   * awaiting session init and re-checks afterwards: a stop that lands while
   * init is in flight cannot clean up state that does not exist yet, so the
   * starter has to notice it was superseded and release the session itself.
   */
  private stopGeneration: number = 0;
  private paused: boolean = false;
  private lastActivityAt: number = Date.now();
  private droppedEvents: number = 0;
  /**
   * Sampling verdict for this recorder, decided once. Re-rolling per call (the
   * previous behaviour) let a recorded session flip itself off on any later
   * path check or resume.
   */
  private sampleDecision: boolean | null = null;

  private originalPushState: History["pushState"] | null = null;
  private originalReplaceState: History["replaceState"] | null = null;
  private globalListenersAttached: boolean = false;

  private enabled: boolean;

  constructor(options: SessionRecorderOptions = {}) {
    this.basePath = options.basePath || DEFAULT_BASE_PATH;
    this.excludedPaths =
      options.useDefaultExcludedPaths === false
        ? options.excludedPaths || []
        : [...DEFAULT_EXCLUDED_PATHS, ...(options.excludedPaths || [])];
    this.sessionSampleRate = options.sessionSampleRate ?? 1.0;
    this.maskAllInputs = options.maskAllInputs ?? true;
    this.maskTextSelector = options.maskTextSelector || "[data-mask]";
    this.blockClass = options.blockClass || "rr-block";
    this.ignoreClass = options.ignoreClass || "rr-ignore";
    this.maskTextClass = options.maskTextClass || "rr-mask";
    this.ignoreSelector = options.ignoreSelector || "[data-ignore]";
    this.blockSelector = options.blockSelector || "[data-private]";
    this.recordCanvas = options.recordCanvas ?? false;
    this.collectFonts = options.collectFonts ?? true;
    this.batchSize = options.batchSize ?? 100;
    this.flushInterval = options.flushInterval ?? 10000;
    this.idleTimeout = options.idleTimeout ?? 60000;
    this.pauseOnHidden = options.pauseOnHidden ?? true;
    this.checkoutEveryNms = options.checkoutEveryNms ?? 30000;
    this.checkoutEveryNth = options.checkoutEveryNth ?? 200;
    this.maxBufferedEvents = options.maxBufferedEvents ?? 5000;
    this.slimDOMOptions = options.slimDOMOptions || {
      script: false,
      comment: true,
      headFavicon: true,
      headWhitespace: true,
      headMetaDescKeywords: true,
      headMetaSocial: true,
      headMetaRobots: true,
      headMetaHttpEquiv: true,
      headMetaAuthorship: true,
      headMetaVerification: true,
    };
    this.maskInputOptions = options.maskInputOptions || { password: true, email: true };
    this.samplingConfig = {
      scroll: 500,
      media: 800,
      mouseInteraction: {
        MouseUp: false,
        MouseDown: false,
        Click: true,
        ContextMenu: false,
        DblClick: true,
        Focus: true,
        Blur: true,
        TouchStart: false,
        TouchEnd: false,
      },
      mousemove: false,
      input: "last",
      ...(options.sampling || {}),
    };

    this.enabled = options.enabled ?? false;
    if (this.enabled && typeof window !== "undefined") {
      this.start();
    }
  }

  /** Sampling verdict, decided once per recorder and then cached. */
  private passesSampling(): boolean {
    if (this.sampleDecision === null) {
      this.sampleDecision = this.sessionSampleRate >= 1.0 || Math.random() <= this.sessionSampleRate;
    }
    return this.sampleDecision;
  }

  private shouldRecord(): boolean {
    if (!this.enabled || typeof window === "undefined") return false;
    if (matchesAnyPath(window.location.pathname, this.excludedPaths)) return false;
    return this.passesSampling();
  }

  private async initializeSession(): Promise<boolean> {
    if (this.sessionInitialized) return true;
    if (this.initFailed) return false;
    if (this.initRetryCount >= this.maxInitRetries) {
      this.initFailed = true;
      return false;
    }

    this.initRetryCount++;
    const sessionId = generateSessionId();
    this.sessionId = sessionId;

    try {
      const metadata = { sessionId, ...getSessionMetadata() };
      const response = await fetch(`${this.basePath}/${SESSION_RECORDER_ENDPOINT}/init`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(metadata),
      });

      if (response.status === 201) {
        this.sessionInitialized = true;
        this.initRetryCount = 0;
        this.initFailed = false;
        if (typeof localStorage !== "undefined") {
          localStorage.setItem("currentRecordingSessionId", sessionId);
        }
        return true;
      }

      this.sessionId = "";
      if (this.initRetryCount >= this.maxInitRetries) this.initFailed = true;
      return false;
    } catch (error) {
      // eslint-disable-next-line no-console
      console.error("[SessionRecorder] init failed:", error);
      this.sessionId = "";
      if (this.initRetryCount >= this.maxInitRetries) this.initFailed = true;
      return false;
    }
  }

  /**
   * Claim the batch to transmit. A batch still in flight is returned unchanged
   * so a retry is byte-identical to the attempt that failed; otherwise the
   * pending buffer is drained into a new, immutable batch.
   */
  private claimBatch(): InflightBatch | null {
    if (this.inflight) return this.inflight;
    if (this.pending.length === 0) return null;
    this.inflight = {
      batchId: generateBatchId(),
      events: this.pending.splice(0, this.pending.length),
    };
    return this.inflight;
  }

  private async sendEvents(isReliable = false): Promise<void> {
    if (!this.sessionInitialized) return;
    if (this.isSending && !isReliable) return;

    const now = Date.now();
    if (this.sendRetryCount > 0 && !isReliable) {
      const backoff = Math.min(1000 * Math.pow(2, this.sendRetryCount), 30000);
      if (now - this.lastSendAttempt < backoff) return;
    }

    const batch = this.claimBatch();
    if (!batch) return;

    this.isSending = true;
    this.lastSendAttempt = now;

    try {
      const packed = pack(batch.events as unknown as Parameters<typeof pack>[0]);
      const encodedEvents = btoa(packed);
      const payload = {
        sessionId: this.sessionId,
        batchId: batch.batchId,
        events: encodedEvents,
      };
      const url = `${this.basePath}/${SESSION_RECORDER_ENDPOINT}/events`;

      if (isReliable) {
        // sendBeacon reports queueing, not delivery, so there is no ack to wait
        // for: release the batch and accept the small risk of loss on unload.
        if (navigator.sendBeacon) {
          const blob = new Blob([JSON.stringify(payload)], { type: "application/json" });
          if (!navigator.sendBeacon(url, blob)) {
            await fetch(url, {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify(payload),
              keepalive: true,
            });
          }
        } else {
          await fetch(url, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(payload),
            keepalive: true,
          });
        }
        this.inflight = null;
        this.sendRetryCount = 0;
      } else {
        const response = await fetch(url, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });

        if (response.status === 404) {
          this.clearSession();
          this.stopRecording();
          this.inflight = null;
          this.pending = [];
          this.sendRetryCount = 0;
        } else if (!response.ok) {
          this.sendRetryCount++;
          // The batch stays in flight so the next attempt resends it verbatim.
          if (this.sendRetryCount >= this.maxSendRetries) {
            this.inflight = null;
            this.sendRetryCount = 0;
          }
        } else {
          this.inflight = null;
          this.sendRetryCount = 0;
        }
      }
    } catch (error) {
      // eslint-disable-next-line no-console
      console.error("[SessionRecorder] send failed:", error);
      this.sendRetryCount++;
      if (this.sendRetryCount >= this.maxSendRetries) {
        this.inflight = null;
        this.sendRetryCount = 0;
      }
    } finally {
      this.isSending = false;
    }
  }

  /** True while there is anything left to transmit. */
  private hasBufferedEvents(): boolean {
    return this.pending.length > 0 || this.inflight !== null;
  }

  private startFlushTimer(): void {
    if (this.flushTimer) return;
    this.flushTimer = setInterval(() => {
      if (this.hasBufferedEvents()) void this.sendEvents(false);
    }, this.flushInterval);
  }

  private stopFlushTimer(): void {
    if (!this.flushTimer) return;
    clearInterval(this.flushTimer);
    this.flushTimer = null;
  }

  private startIdleTimer(): void {
    if (this.idleTimer || this.idleTimeout <= 0) return;
    const tick = Math.min(this.idleTimeout, IDLE_CHECK_CEILING_MS);
    this.idleTimer = setInterval(() => {
      if (this.paused || !this.stopFn) return;
      if (Date.now() - this.lastActivityAt >= this.idleTimeout) this.pauseRecording();
    }, tick);
  }

  private stopIdleTimer(): void {
    if (!this.idleTimer) return;
    clearInterval(this.idleTimer);
    this.idleTimer = null;
  }

  private async startRecording(): Promise<void> {
    // `stopFn` is only assigned after the await below, so it cannot on its own
    // keep a second caller out of this function. Without `starting`, anything
    // that calls in during session init — a framework router touching
    // `replaceState`, say — attaches a second rrweb recorder to the same page
    // and every event is captured twice.
    if (this.stopFn || this.initFailed || this.starting) return;
    if (!this.shouldRecord()) return;

    this.starting = true;
    const generation = this.stopGeneration;
    try {
      const ok = await this.initializeSession();
      if (!ok) return;

      // A stop()/destroy() during init — a StrictMode double-mount, or the
      // React binding's effect cleanup — returned before this session existed,
      // so it could not tear it down. Release it here instead of leaving a
      // server-side session that never receives an event.
      if (this.stopGeneration !== generation || this.stopFn || !this.enabled) {
        this.clearSession();
        return;
      }

      const stopFn = record({
        emit: (event: eventWithTime) => {
          this.pending.push(event);
          if (this.pending.length > this.maxBufferedEvents) {
            const overflow = this.pending.length - this.maxBufferedEvents;
            this.pending.splice(0, overflow);
            this.droppedEvents += overflow;
          }
          if (this.pending.length >= this.batchSize) void this.sendEvents(false);
        },
        sampling: this.samplingConfig,
        blockSelector: this.blockSelector,
        ignoreSelector: this.ignoreSelector,
        recordCanvas: this.recordCanvas,
        collectFonts: this.collectFonts,
        maskAllInputs: this.maskAllInputs,
        maskInputOptions: this.maskInputOptions,
        maskTextSelector: this.maskTextSelector,
        blockClass: this.blockClass,
        ignoreClass: this.ignoreClass,
        maskTextClass: this.maskTextClass,
        slimDOMOptions: this.slimDOMOptions,
        checkoutEveryNms: this.checkoutEveryNms,
        checkoutEveryNth: this.checkoutEveryNth,
      });

      if (stopFn) {
        this.stopFn = stopFn;
        const exposed = stopFn as unknown as Record<string, unknown>;
        this.takeSnapshot = (exposed.takeFullSnapshot as () => void) || null;
      }

      this.paused = false;
      this.lastActivityAt = Date.now();
      this.startFlushTimer();
      this.startIdleTimer();
    } finally {
      this.starting = false;
    }
  }

  /** Forget the server session and any client state pointing at it. */
  private clearSession(): void {
    this.sessionInitialized = false;
    this.sessionId = "";
    if (typeof localStorage !== "undefined") {
      localStorage.removeItem("currentRecordingSessionId");
    }
  }

  private detachRecorder(): void {
    if (!this.stopFn) return;
    this.stopFn();
    this.stopFn = null;
    this.takeSnapshot = null;
  }

  /**
   * Suspend recording without ending the session. rrweb is detached so a
   * passive page emits nothing at all; `resumeRecording` reattaches and rrweb
   * emits a fresh full snapshot, which is what replay needs to resync after
   * the gap.
   */
  private pauseRecording(): void {
    if (this.paused || !this.stopFn) return;
    this.detachRecorder();
    this.paused = true;
    this.stopFlushTimer();
    if (this.hasBufferedEvents()) void this.sendEvents(false);
  }

  private resumeRecording(): void {
    if (!this.paused || !this.enabled || this.initFailed) return;
    this.paused = false;
    void this.startRecording();
  }

  private stopRecording(): void {
    // `starting` and `sessionInitialized` are part of the condition: guarding
    // on `stopFn` alone skipped cleanup entirely while init was in flight,
    // stranding the session it was about to create.
    if (!this.stopFn && !this.paused && !this.starting && !this.sessionInitialized) return;
    this.stopGeneration++;
    this.detachRecorder();
    this.stopFlushTimer();
    this.stopIdleTimer();
    this.paused = false;
    void this.sendEvents(true);
    this.clearSession();
    this.initRetryCount = 0;
    this.initFailed = false;
  }

  private handleActivity = (): void => {
    this.lastActivityAt = Date.now();
    if (this.paused) this.resumeRecording();
  };

  private handleVisibilityChange = (): void => {
    if (!this.pauseOnHidden) return;
    if (document.visibilityState === "hidden") {
      this.pauseRecording();
    } else {
      this.handleActivity();
    }
  };

  private handleUnload = (): void => {
    if (this.sessionInitialized && this.hasBufferedEvents()) {
      void this.sendEvents(true);
    }
  };

  private attachGlobalListeners(): void {
    if (this.globalListenersAttached) return;
    this.globalListenersAttached = true;

    this.originalPushState = window.history.pushState;
    this.originalReplaceState = window.history.replaceState;

    const flushAndCheck = (): void => {
      if (this.stopFn && this.hasBufferedEvents()) void this.sendEvents(false);
      setTimeout(() => {
        this.checkPath();
        if (this.takeSnapshot) this.takeSnapshot();
      }, 100);
    };

    window.history.pushState = ((...args: Parameters<History["pushState"]>) => {
      this.originalPushState?.apply(window.history, args);
      flushAndCheck();
    }) as History["pushState"];

    window.history.replaceState = ((...args: Parameters<History["replaceState"]>) => {
      this.originalReplaceState?.apply(window.history, args);
      flushAndCheck();
    }) as History["replaceState"];

    window.addEventListener("popstate", this.handlePopState);
    window.addEventListener("beforeunload", this.handleUnload);
    window.addEventListener("pagehide", this.handleUnload);
    document.addEventListener("visibilitychange", this.handleVisibilityChange);
    // These stay attached while paused — they are how a paused recorder learns
    // the visitor came back.
    for (const name of ACTIVITY_EVENTS) {
      window.addEventListener(name, this.handleActivity, { passive: true, capture: true });
    }
  }

  private detachGlobalListeners(): void {
    if (!this.globalListenersAttached) return;
    this.globalListenersAttached = false;

    if (this.originalPushState) {
      window.history.pushState = this.originalPushState;
      this.originalPushState = null;
    }
    if (this.originalReplaceState) {
      window.history.replaceState = this.originalReplaceState;
      this.originalReplaceState = null;
    }
    window.removeEventListener("popstate", this.handlePopState);
    window.removeEventListener("beforeunload", this.handleUnload);
    window.removeEventListener("pagehide", this.handleUnload);
    document.removeEventListener("visibilitychange", this.handleVisibilityChange);
    for (const name of ACTIVITY_EVENTS) {
      window.removeEventListener(name, this.handleActivity, { capture: true });
    }
  }

  private handlePopState = (): void => {
    if (this.stopFn && this.hasBufferedEvents()) void this.sendEvents(false);
    setTimeout(() => {
      this.checkPath();
      if (this.takeSnapshot) this.takeSnapshot();
    }, 100);
  };

  private checkPath(): void {
    if (!this.enabled || this.initFailed) {
      if (!this.enabled) this.stopRecording();
      return;
    }
    const isExcluded = matchesAnyPath(window.location.pathname, this.excludedPaths);
    const isRecording = this.stopFn !== null;
    if (isExcluded && isRecording) {
      this.stopRecording();
    } else if (!isExcluded && !isRecording && !this.paused) {
      void this.startRecording();
    }
  }

  public start(): void {
    if (this.enabled && this.stopFn) return;
    this.enabled = true;
    if (typeof window === "undefined") return;
    this.attachGlobalListeners();
    void this.startRecording();
  }

  public stop(): void {
    this.enabled = false;
    this.stopRecording();
  }

  public destroy(): void {
    this.stop();
    this.stopIdleTimer();
    this.detachGlobalListeners();
  }

  public getSessionId(): string | null {
    return this.sessionId || null;
  }

  /** Events discarded because the buffer hit `maxBufferedEvents`. */
  public getDroppedEventCount(): number {
    return this.droppedEvents;
  }

  /** Whether recording is currently suspended for idleness or a hidden tab. */
  public isPaused(): boolean {
    return this.paused;
  }
}
