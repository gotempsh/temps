# CI automation

Use a pinned `@temps-sdk/cli@0.1.32` invocation and the immutable package
integrity check from [the CLI runtime](../references/cli-runtime.md).

- Store `TEMPS_TOKEN` and `TEMPS_API_URL` in the CI provider's secret store.
- Use a dedicated least-privilege identity and named `ci` context; never copy a
  developer's local `~/.temps` directory into CI.
- Run `whoami` and a read-only project query against the explicit context before
  mutation.
- Pin the package version, runner version, source revision, project,
  environment, and context. Do not rely on ambient “current” state.
- Separate destructive recovery/cleanup jobs from ordinary deployment jobs and
  protect them with manual approval.
- Keep debug output off around auth and credential operations. Redact logs and
  retain non-secret deployment IDs/status as evidence.
- After deployment, poll a bounded status endpoint and test the deployed health
  URL. Fail CI if the deployment never becomes healthy.
