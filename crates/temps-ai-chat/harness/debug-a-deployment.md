---
name: debug-a-deployment
description: A deployment failed, or the app errors or does not answer after deploying
contexts: assistant, project
---

# Debug a deployment

Use this when a deployment failed, a goal brought you back because one did, or
the deployed app errors or does not answer.

1. Find the stage that failed before you change anything. Read
   `get_deployment_jobs` for the deployment: the first job that is not
   completed is the stage (download or upload, build, image, start, health
   check). Read that job with `get_deployment_job_logs`. When every job
   completed but the app misbehaves, read `get_container_logs` instead.
2. Name the cause in one sentence, from what the logs say. The usual ones:
   - The server listens on `localhost` or a fixed port instead of `0.0.0.0`
     and `$PORT`: the health check never reaches it.
   - A worker has no HTTP listener: give it a small `GET /health` on `$PORT`.
   - A variable is missing: the service is not linked to this project, or the
     value was never set in the project's environment variables.
   - The build queries the database (for example during `next build`): move
     the query to request time.
   - A dependency is missing from the manifest, or the lockfile does not match
     it: the sandbox had it installed by hand.
   - Files are missing: folders named `dist`, `build`, `.next`, `target` or
     `coverage` are never uploaded, at any depth, so the build must create
     them on Temps.
   - The wrong preset: the build or start command does not fit the code.
3. Make the smallest fix, in the sandbox when the code came from one, and run
   the same check there that failed on Temps (the build, or a `curl` of the
   route).
4. Deploy again (`temps_deploy_sandbox_source` for code from the sandbox,
   `trigger_project_pipeline` for a Git project). If a goal is watching it,
   call `temps_goal` with `action` `retarget` and the new check.
5. Never repeat an unchanged call that already failed. After two fixes that
   did not work, list the likely causes for the user and say which you will
   try next, or what only they can provide.
