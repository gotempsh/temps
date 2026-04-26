# Changelog — PR #64

**Title:** fix(git): return 409 when GitHub repo name already exists
**Branch:** `fix/git-repo-already-exists-409` → `main`
**URL:** https://github.com/gotempsh/temps/pull/64
**Commit:** `11bb63f`

## Fixed

- **GitHub repo name conflicts now return 409 Conflict instead of a generic 500.**
  Detect GitHub's `422 name already exists` response and surface it as a typed
  `GitProviderError::RepositoryAlreadyExists { name }` variant. The handler maps
  this to **409 Conflict** with an actionable detail message, so the frontend
  can render meaningful feedback to the user.

- **Stop double-wrapping `GitProviderManagerError::ProviderError`.**
  The typed `Problem` is now forwarded directly through
  `temps-projects` handlers instead of being collapsed into a 500.

- **Frontend ProblemDetails extraction is now spec-compliant and envelope-aware.**
  `extractProblemDetails` was rewritten to:
  1. Use the correct RFC 7807 field name `type` (previously `type_url`).
  2. Unwrap `body` / `error` / `data` envelopes used by the hey-api openapi-ts client.
  3. Gate detection on a proper type guard so unrelated thrown objects are
     no longer misidentified as ProblemDetails.

## Files changed

| File | Change |
|------|--------|
| `crates/temps-git/src/services/git_provider.rs` | New `RepositoryAlreadyExists { name }` error variant |
| `crates/temps-git/src/services/github_provider.rs` | Parse 422 body, detect `name` field "already exists" |
| `crates/temps-git/src/handlers/base.rs` | Map variant to 409, drop redundant wrapping |
| `crates/temps-projects/src/handlers/handlers.rs` | Propagate typed Problem instead of converting to 500 |
| `web/src/utils/errorHandling.ts` | Fix RFC 7807 field name and envelope unwrapping |

## Test plan

- [ ] Create a repository through the project flow with a name that already
      exists on the linked GitHub account → expect a 409 with a
      "Repository Already Exists" toast/banner instead of a 500.
- [ ] Create a repository with a fresh name → expect success unchanged.
- [ ] Hit any other ProblemDetails-returning endpoint and confirm the
      frontend still extracts `title` / `detail` correctly through both the
      bare and `body`-nested shapes.

## User-facing impact

Before: creating a project on GitHub with a duplicate repo name resulted in an
opaque "Internal Server Error" and the user had no idea what went wrong.

After: the user sees a clear 409 with the conflicting name, can choose a
different name, and retry without contacting support.
