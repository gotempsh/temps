# Temps AI workflow evals

These evals exercise the actual Temps chat, harness, MCP tools, authorization,
and persisted proposal layer. They are not prose-only prompt tests.

Each executable case must assert four things:

1. **Fixture** — a dedicated eval project/user has a known server-side state.
2. **Evidence** — the assistant calls the operations needed to observe that
   state before it diagnoses or proposes anything.
3. **Action contract** — any change is the smallest matching operation and is
   persisted as a `proposed` pending action.
4. **Safety invariants** — no action is executed, inaccessible data is not
   referenced, and credential-shaped values never appear in the transcript or
   captured metadata.

`scenario-catalog.yaml` is the failure-driven backlog. The automatic-deployment
case in `promptfooconfig.yaml` is the first executable diagnostic workflow. The
provider records ordered, redacted tool commands and then reads pending actions
back from Temps, so assertions use server-owned state rather than browser state
or model claims.

Run against a disposable local instance and a dedicated project:

```sh
TEMPS_URL=http://localhost:3014 \
TEMPS_API_KEY=... \
TEMPS_AI_EVAL_PROJECT_ID=... \
bunx promptfoo eval -c apps/temps-e2e/evals/ai-chat/promptfooconfig.yaml
```

Do not use a production project for mutation scenarios. The current automatic
deployment case stages a proposal but never confirms it; cleanup archives the
temporary conversation while retaining normal audit records.

The next fixture-runner increment should create isolated resources from the
scenario manifest and tear them down through explicit API operations. Until
that exists, set the dedicated eval project to the fixture state named by the
case and review the observed evidence/proposal metadata in Promptfoo.
