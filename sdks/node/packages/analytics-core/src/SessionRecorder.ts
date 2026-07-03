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

function generateSessionId(): string {
  if (typeof crypto !== "undefined" && crypto.randomUUID) {
    return crypto.randomUUID();
  }
  return `session_${Date.now()}_${Math.random().toString(36).substring(2, 11)}`;
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
  private readonly slimDOMOptions: Record<string, boolean>;
  private readonly maskInputOptions: { password?: boolean; email?: boolean };
  private readonly samplingConfig: Record<string, unknown>;

  private stopFn: (() => void) | null = null;
  private takeSnapshot: (() => void) | null = null;
  private events: eventWithTime[] = [];
  private sessionId: string = "";
  private sessionInitialized: boolean = false;
  private flushTimer: ReturnType<typeof setTimeout> | null = null;
  private initRetryCount: number = 0;
  private initFailed: boolean = false;
  private readonly maxInitRetries: number = 3;
  private sendRetryCount: number = 0;
  private readonly maxSendRetries: number = 5;
  private lastSendAttempt: number = 0;
  private isSending: boolean = false;

  private originalPushState: History["pushState"] | null = null;
  private originalReplaceState: History["replaceState"] | null = null;

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

  private shouldRecord(): boolean {
    if (!this.enabled || typeof window === "undefined") return false;
    const currentPath = window.location.pathname;
    const isExcluded = matchesAnyPath(currentPath, this.excludedPaths);
    if (isExcluded) return false;
    if (this.sessionSampleRate < 1.0 && Math.random() > this.sessionSampleRate) return false;
    return true;
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

  private async sendEvents(isReliable = false): Promise<void> {
    if (!this.sessionInitialized || this.events.length === 0) return;
    if (this.isSending && !isReliable) return;

    const now = Date.now();
    if (this.sendRetryCount > 0 && !isReliable) {
      const backoff = Math.min(1000 * Math.pow(2, this.sendRetryCount), 30000);
      if (now - this.lastSendAttempt < backoff) return;
    }

    this.isSending = true;
    this.lastSendAttempt = now;
    const eventsToSend = [...this.events];

    try {
      const packed = pack(eventsToSend as unknown as Parameters<typeof pack>[0]);
      const encodedEvents = btoa(packed);
      const payload = { sessionId: this.sessionId, events: encodedEvents };
      const url = `${this.basePath}/${SESSION_RECORDER_ENDPOINT}/events`;

      if (isReliable) {
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
        this.events = [];
        this.sendRetryCount = 0;
      } else {
        const response = await fetch(url, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });

        if (response.status === 404) {
          this.sessionInitialized = false;
          this.sessionId = "";
          if (typeof localStorage !== "undefined") {
            localStorage.removeItem("currentRecordingSessionId");
          }
          this.stopRecording();
          this.events = [];
          this.sendRetryCount = 0;
        } else if (!response.ok) {
          this.sendRetryCount++;
          if (this.sendRetryCount >= this.maxSendRetries) {
            this.events = [];
            this.sendRetryCount = 0;
          }
        } else {
          this.events = [];
          this.sendRetryCount = 0;
        }
      }
    } catch (error) {
      // eslint-disable-next-line no-console
      console.error("[SessionRecorder] send failed:", error);
      this.sendRetryCount++;
      if (this.sendRetryCount >= this.maxSendRetries) {
        this.events = [];
        this.sendRetryCount = 0;
      }
    } finally {
      this.isSending = false;
    }
  }

  private scheduleFlush(): void {
    if (this.flushTimer) clearTimeout(this.flushTimer);
    this.flushTimer = setTimeout(() => {
      if (this.events.length > 0) void this.sendEvents(false);
      this.scheduleFlush();
    }, this.flushInterval);
  }

  private async startRecording(): Promise<void> {
    if (this.stopFn || this.initFailed) return;
    if (!this.shouldRecord()) return;

    const ok = await this.initializeSession();
    if (!ok) return;

    const stopFn = record({
      emit: (event: eventWithTime) => {
        this.events.push(event);
        if (this.events.length >= this.batchSize) {
          void this.sendEvents(false);
          this.scheduleFlush();
        }
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
      checkoutEveryNms: 30000,
      checkoutEveryNth: 200,
    });

    if (stopFn) {
      this.stopFn = stopFn;
      const exposed = stopFn as unknown as Record<string, unknown>;
      this.takeSnapshot = (exposed.takeFullSnapshot as () => void) || null;
    }

    this.scheduleFlush();
  }

  private stopRecording(): void {
    if (!this.stopFn) return;
    this.stopFn();
    this.stopFn = null;
    if (this.flushTimer) {
      clearTimeout(this.flushTimer);
      this.flushTimer = null;
    }
    void this.sendEvents(true);
    this.sessionInitialized = false;
    this.sessionId = "";
    if (typeof localStorage !== "undefined") {
      localStorage.removeItem("currentRecordingSessionId");
    }
    this.initRetryCount = 0;
    this.initFailed = false;
  }

  private handleUnload = (): void => {
    if (this.sessionInitialized && this.events.length > 0) {
      void this.sendEvents(true);
    }
  };

  private wrapHistory(): void {
    this.originalPushState = window.history.pushState;
    this.originalReplaceState = window.history.replaceState;

    const flushAndCheck = (): void => {
      if (this.stopFn && this.events.length > 0) void this.sendEvents(false);
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
  }

  private handlePopState = (): void => {
    if (this.stopFn && this.events.length > 0) void this.sendEvents(false);
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
    const currentPath = window.location.pathname;
    const isExcluded = matchesAnyPath(currentPath, this.excludedPaths);
    const isRecording = this.stopFn !== null;
    if (isExcluded && isRecording) {
      this.stopRecording();
    } else if (!isExcluded && !isRecording && !this.initFailed) {
      void this.startRecording();
    }
  }

  public start(): void {
    if (this.enabled && this.stopFn) return;
    this.enabled = true;
    if (typeof window === "undefined") return;
    if (!this.originalPushState) this.wrapHistory();
    void this.startRecording();
  }

  public stop(): void {
    this.enabled = false;
    this.stopRecording();
  }

  public destroy(): void {
    this.stop();
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
  }

  public getSessionId(): string | null {
    return this.sessionId || null;
  }
}
