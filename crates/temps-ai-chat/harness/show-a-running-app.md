---
name: show-a-running-app
description: Show the user an app or server that is running in the sandbox
requires_tool: sandbox_preview
contexts: assistant, project
---

# Show a running app

Use this when the user wants to see, open or try something that runs in the
sandbox.

1. First check that nothing already serves the port
   (`curl -s -o /dev/null -w '%{http_code}' localhost:3000`): never start a
   second dev server on top of the first. Then start it in the background,
   bound to every interface, with the framework's own flags, for example
   `nohup npm run dev -- -H 0.0.0.0 -p 3000 > /tmp/dev.log 2>&1 &` for Next.js,
   `--host 0.0.0.0 --port 3000` for Vite or uvicorn. A server bound to
   `localhost` cannot be reached from the preview. Use 3000, 3001, 3002… when
   several parts run at once. To stop one, kill its PID (`ss -ltnp` shows it),
   never `pkill -f node`.
2. Call `sandbox_preview` with the port. It returns the address and checks that
   the server is listening and answers HTTP. It also opens the page in the
   chat's Preview tab beside the conversation, so call it every time you say a
   page is running, even when you already know the address.
3. When `ready` is true, give the user the `url` it returned. When it is false,
   do what `advice` says, then call it again. A dev server can need a few
   seconds to start.
4. Use only that address. Never invent another host or port, and never use
   8080 or 8090 as the app's port: those belong to Temps.
5. Never use a tunnel or any outside service to publish the sandbox
   (Cloudflare, ngrok, localtunnel and the like). `sandbox_exec` refuses them,
   and the preview already works without one.

The preview lasts as long as the sandbox. For an address that stays, publish
the code as a Temps project, as described in the `build-an-app` playbook.
