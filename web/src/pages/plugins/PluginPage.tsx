import { usePluginsContext } from '@/contexts/PluginsContext'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import { Loader2 } from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { useLocation, useNavigate, useParams } from 'react-router'

/**
 * Renders an external plugin's UI inside an iframe.
 *
 * The plugin serves its own HTML/JS/CSS at `/api/x/{pluginName}/ui/`.
 *
 * ## Deep linking
 *
 * A plugin's own route is mirrored into the console's address bar, so
 * `/plugins/vibetemps/app/21` is a real, copyable link that reopens the
 * plugin exactly where it was. Without this every plugin URL collapsed to
 * `/plugins/vibetemps`, and sharing "look at this app" meant sharing a
 * screenshot and a sentence of directions.
 *
 * Two mechanisms, because plugins route in two different ways:
 *
 * 1. **Hash routers, zero configuration.** The iframe is same-origin (it is
 *    served from `/api/x/...` on this origin), so the console can read and
 *    write `contentWindow.location.hash` directly and listen for
 *    `hashchange`. A plugin using a hash router — the common choice for
 *    something mounted under an arbitrary prefix — gets deep linking without
 *    knowing this exists.
 *
 * 2. **`postMessage`, for everything else.** A plugin using history routing
 *    (or one that simply wants control) posts
 *    `{ type: 'temps:route', path: 'app/21' }` and receives
 *    `{ type: 'temps:navigate', path }` when the user goes back or forward.
 *
 * Both are guarded against feedback loops by `syncedPathRef`: whichever side
 * moved last records the path, and the opposite direction ignores it.
 */
export function PluginPage() {
  const { pluginName, '*': pluginPath } = useParams<{
    pluginName: string
    '*': string
  }>()
  const { getPlugin } = usePluginsContext()
  const navigate = useNavigate()
  const location = useLocation()
  const iframeRef = useRef<HTMLIFrameElement>(null)
  const [isLoading, setIsLoading] = useState(true)
  const [hasError, setHasError] = useState(false)

  const plugin = pluginName ? getPlugin(pluginName) : undefined

  /**
   * The path last propagated in either direction.
   *
   * Both syncs write it before acting, so the change they cause arrives back
   * as a no-op instead of bouncing forever between the console's router and
   * the plugin's.
   */
  const syncedPathRef = useRef<string | undefined>(pluginPath)

  /**
   * Whether this plugin has declared its route over `postMessage`.
   *
   * Latches on the first `temps:route` and never clears: a plugin does not
   * switch routing strategy mid-session, and treating it as a one-way door is
   * what keeps the hash listener from clobbering an explicitly-sent path.
   */
  const routesByMessageRef = useRef(false)

  /**
   * The sub-route this page was opened at, frozen at mount.
   *
   * The iframe `src` is derived from it and must never be recomputed as the
   * route changes: assigning `src` reloads the document, so mirroring a
   * navigation back into it would restart the plugin — losing an in-flight
   * chat, a half-filled form, a running build — on every click. Later
   * navigation goes through `location.hash` or `postMessage`, neither of
   * which reloads.
   *
   * State rather than a ref because the manifest list is still loading on the
   * first render, so the value has to survive until `plugin` resolves, and
   * reading a ref during render is what actually renders the `src`.
   */
  const [initialPath] = useState(() => pluginPath ?? '')

  /** The console URL for a path inside this plugin. */
  const consoleUrlFor = useCallback(
    (path: string) =>
      path ? `/plugins/${pluginName}/${path}` : `/plugins/${pluginName}`,
    [pluginName]
  )

  /**
   * Plugin → console. Mirror the plugin's route into the address bar.
   *
   * `replace`, not `push`: an iframe navigation already contributes to the
   * joint session history, so pushing here would make the browser's back
   * button need two presses for one step inside the plugin.
   */
  const adoptPluginPath = useCallback(
    (path: string) => {
      if (path === syncedPathRef.current) return
      syncedPathRef.current = path
      navigate(consoleUrlFor(path), { replace: true })
    },
    [consoleUrlFor, navigate]
  )

  // ---- 1. Hash routers: read the iframe's own location -------------------
  useEffect(() => {
    const win = iframeRef.current?.contentWindow
    if (!win) return

    const readHash = () => {
      try {
        return win.location.hash.replace(/^#\/?/, '')
      } catch {
        // Cross-origin: a plugin served from somewhere other than this
        // origin. Not an error — it simply has to use `postMessage`.
        return undefined
      }
    }

    const onHashChange = () => {
      // A plugin that has spoken `temps:route` owns its routing; its hash is
      // not where its route lives, and reading it would fight the messages.
      if (routesByMessageRef.current) return
      const path = readHash()
      if (path !== undefined) adoptPluginPath(path)
    }

    // Snapshot on attach, because the plugin may have redirected during boot
    // (a bare `/ui/` landing on `#/apps`) and that destination is what belongs
    // in the address bar.
    //
    // Only a non-empty hash, though. This effect re-runs whenever it is
    // re-attached, and an empty hash carries no routing information — so
    // snapshotting one would reset the URL to the bare plugin route every
    // time, silently undoing a path that arrived over `postMessage`. A
    // *hashchange event* to empty is different: that is a hash router
    // genuinely navigating to its root, and `onHashChange` still honours it.
    const settled = readHash()
    if (!routesByMessageRef.current && settled) adoptPluginPath(settled)

    win.addEventListener('hashchange', onHashChange)
    return () => win.removeEventListener('hashchange', onHashChange)
    // Re-attached after every load: navigating the iframe replaces its
    // window, and listeners do not survive that.
  }, [adoptPluginPath, isLoading])

  // ---- 2. postMessage: for plugins that route without a hash -------------
  useEffect(() => {
    const onMessage = (event: MessageEvent) => {
      // Same-origin only. The iframe is served from this origin, so anything
      // from elsewhere is not our plugin and must not be able to steer the
      // console's address bar.
      if (event.origin !== window.location.origin) return
      if (event.source !== iframeRef.current?.contentWindow) return

      const data = event.data as { type?: string; path?: string } | null
      if (data?.type !== 'temps:route' || typeof data.path !== 'string') return

      // From here on this plugin routes explicitly, so the hash listener
      // stands down. The two channels are alternatives, not a fallback chain:
      // a history-router plugin leaves its hash permanently empty, and letting
      // that empty value keep mirroring would erase every route it sends.
      routesByMessageRef.current = true
      adoptPluginPath(data.path.replace(/^\/+/, ''))
    }

    window.addEventListener('message', onMessage)
    return () => window.removeEventListener('message', onMessage)
  }, [adoptPluginPath])

  // ---- 3. Console → plugin: back, forward, and pasted links --------------
  useEffect(() => {
    const path = pluginPath ?? ''
    if (path === (syncedPathRef.current ?? '')) return
    syncedPathRef.current = path

    const win = iframeRef.current?.contentWindow
    if (!win) return

    // Tell a postMessage-based plugin either way; it is a no-op for one that
    // does not listen, and the fragment navigation below is a no-op for one
    // that does not use hashes.
    win.postMessage({ type: 'temps:navigate', path }, window.location.origin)
    try {
      // `location.replace` rather than assigning `location.hash`. A URL that
      // differs only in its fragment is a same-document navigation either
      // way — no reload — but `replace` overwrites the iframe's current
      // history entry instead of pushing a new one. That matches the
      // `replace: true` used on the console side, so one user-visible move
      // stays one press of the back button rather than two.
      const target = new URL(win.location.href)
      target.hash = path ? `#/${path}` : ''
      if (target.href !== win.location.href) win.location.replace(target.href)
    } catch {
      // Cross-origin plugin: the postMessage above was the only channel.
    }
  }, [pluginPath, location.key])

  // Declared here, not inline in the JSX below: everything after this point is
  // past the `if (!plugin)` early return, and a hook called there runs only on
  // the renders where a plugin happens to be resolved. React counts hooks
  // positionally, so the first render that finds one blows up the whole tree —
  // which shows up as a blank page, not an error boundary.
  const handleLoad = useCallback(() => setIsLoading(false), [])
  const handleError = useCallback(() => {
    setIsLoading(false)
    setHasError(true)
  }, [])

  if (!plugin) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[400px] text-muted-foreground">
        <p>Plugin not found: {pluginName}</p>
      </div>
    )
  }

  const Icon = resolvePluginIcon(plugin.nav[0]?.icon ?? 'puzzle')
  const displayName = plugin.display_name ?? plugin.name
  const base = `/api/x/${plugin.name}/ui/`
  const iframeSrc = initialPath ? `${base}#/${initialPath}` : base

  return (
    // `h-full`, not `h-[calc(100vh-4rem)]`. The old value assumed exactly
    // one 4rem strip of console chrome above; the disk-space and
    // update-available banners are dismissible and stack, so whenever one
    // showed, the frame ran past the bottom of the window and the plugin
    // lost its last rows — for a chat UI, that is the composer.
    <div className="flex flex-col h-full min-h-0">
      {/* Header, unless the plugin renders its own — see
          `PluginManifest::hide_header`. */}
      {!plugin.hide_header && (
        <div className="flex items-center gap-2 px-4 py-2 border-b bg-background shrink-0">
          <div className="flex h-7 w-7 items-center justify-center rounded-md bg-muted">
            <Icon className="h-4 w-4" />
          </div>
          <h1 className="text-sm font-medium">{displayName}</h1>
          <span className="text-xs text-muted-foreground font-mono">
            v{plugin.version}
          </span>
          {isLoading && (
            <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground ml-auto" />
          )}
        </div>
      )}

      {/* iframe container */}
      {hasError ? (
        <div className="flex flex-col items-center justify-center flex-1 text-muted-foreground gap-2">
          <p className="text-sm">
            Failed to load plugin UI from{' '}
            <code className="rounded bg-muted px-1.5 py-0.5 text-xs font-mono">
              {iframeSrc}
            </code>
          </p>
          <p className="text-xs">
            Make sure the plugin serves HTML at its <code>/ui/</code> endpoint.
          </p>
        </div>
      ) : (
        <iframe
          ref={iframeRef}
          src={iframeSrc}
          title={`${displayName} plugin`}
          className="flex-1 min-h-0 w-full border-0"
          onLoad={handleLoad}
          onError={handleError}
          // `allow-top-navigation-by-user-activation` lets a plugin link the
          // user back into the console — a plugin whose feature depends on
          // operator configuration has to be able to say "set it up here"
          // and have the link work. Without it such links silently do
          // nothing, which reads as a broken button.
          //
          // The `-by-user-activation` form, not the blanket one: navigation
          // is only permitted as the direct result of a click, so a plugin
          // cannot redirect the console on its own.
          sandbox="allow-scripts allow-forms allow-same-origin allow-popups allow-top-navigation-by-user-activation"
        />
      )}
    </div>
  )
}
