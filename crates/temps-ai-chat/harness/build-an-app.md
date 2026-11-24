---
name: build-an-app
description: Design, build, check and publish a new app, site, API or worker, or change one
contexts: assistant, project
---

# Build an app

Use this when the user wants an app, site, API or worker built or changed.

1. Design first. If you have `temps_propose_blueprint` and this is a new app,
   or the change adds or removes a part (an app, a database, a broker, an
   outside service) or a flow between parts, propose the architecture and end
   your turn; build nothing until the user accepts it. A change inside an
   existing part (a fix, a page, a route) needs no proposal. Without the tool,
   state the plan in at most five lines and start.
2. Build it in the sandbox. If you have `temps_create_sandbox`, call it right
   away without asking, say in one sentence that you did, and carry on.
   Otherwise `temps_offer_sandbox` asks the user once. In a project chat,
   create it right away so you can read the code. With no sandbox tools at all,
   say what you would build and stop.
3. Write the code with `sandbox_write_file` and run commands with
   `sandbox_exec`. Keep one project flat at the top of the sandbox work
   directory (`package.json`, `index.html` and `src/` right there),
   not in a folder named after the app. Only when the application has several app
   parts does each get its own folder, named by its part id (`web/`, `api/`,
   `worker/`). Write every file of a step in one round of parallel
   `sandbox_write_file` calls, and install and build in one `sandbox_exec`
   (`npm install && npm run build`). Older tool results are trimmed, so read a
   file again before editing it rather than recalling it.
   If it has pages people look at, read the `build-a-website` playbook
   before writing them, and the `track-app-usage` playbook: every app with
   pages sends Temps analytics and named events for what visitors do, so
   plan the events with the pages and wire them before the first deploy.
   Read the `add-temps-observability` playbook too: wire error tracking and
   traces to Temps when the code already uses Sentry or OpenTelemetry, and
   otherwise offer them at the end instead of adding them.
4. Check it before you say it works, with commands run this turn, and quote
   them: the build exits 0 with no type errors; the server listens on
   `0.0.0.0` at `$PORT` (default 3000); every route you added answers as
   expected to `curl` (show the status and the body); with a database, a row
   you wrote is read back. A timeout or a "sandbox was woken" notice is
   transient: retry once. A non-zero exit is a result: read stderr and fix the
   cause. After two failed fixes for the same error, list the likely causes,
   tell the user, and try the most likely one.
5. Do not look through Git providers or repositories to find somewhere to put
   the code. They matter only once there is working code and the user wants it
   kept in a repository.
6. To show it running, follow the `show-a-running-app` playbook.
7. If a sandbox tool fails, tell the user what failed and why. Do not move the
   work somewhere else because of it.
8. To publish it, use Temps and nothing else, and do not go through Git: the
   files are already in the sandbox. Create one Temps project per app part
   with `source_type` `uploaded_source` and the preset `list_presets` offers
   for it (`nextjs`, `vite`, a Dockerfile, …); say which one you chose. The
   preset decides which browser error-tracking variable the build gets, so
   it must match the framework. Use
   `static_files` only for plain HTML, CSS and JS with no build step: anything
   with a build step is uploaded as source so Temps builds it, because folders
   named `dist`, `build`, `.next`, `target` or `coverage` are never uploaded,
   at any depth. Then call `temps_deploy_sandbox_source` with its `project_id`
   (and `directory` only when the app is one of several projects in their own
   folders), APIs before the front ends that call them; give a front end the
   API's URL as an environment variable. Temps packs the files itself, leaves
   out `node_modules`, `.git`, `.env` files and credentials, asks the user to
   approve, then builds and serves the app with a domain and TLS. Never zip or
   upload files by hand. Put only variable names, never values, in
   `.env.example`. Commit to a repository only when the user asks to keep the
   code in one. Do not suggest another hosting platform.
9. Each database, cache or bucket is a Temps service: read
   `get_service_type_parameters` first, create it with `create_service` and
   the first project's `--project_id`, and attach it to the other parts with
   `link_service_to_project`. If you have `temps_bind_blueprint`, call it after
   each project or service you create, with the part's id.
10. After `temps_deploy_sandbox_source`, call `temps_goal` with the goal it
    returns, say it is building, and end the turn. "Completed" only proves the
    server answered on `/`: when you are next in the chat, request the real
    routes on the deployed URL and read `get_container_logs` for errors. If the
    build failed, read `get_deployment_jobs`, then the failed job's
    `get_deployment_job_logs`, before changing anything. If it built in the
    sandbox but fails on Temps, compare the runtime version, the lockfile and
    the build and start commands.
11. Whenever you stop building, deployed or not, end with all of these:
    - what is live (the URL), or where the user can see it;
    - what you checked;
    - the analytics events it sends: each name and when it fires;
    - whether it reports errors and traces, and when it does not, a one-line
      offer to add them (plain HTML cannot have them: say so instead);
    - what is not done yet, and the next step.
