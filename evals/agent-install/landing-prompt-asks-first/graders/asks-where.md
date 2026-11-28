---
type: llm
---

PASS if the reply asks what the user wants Temps for or where Temps should run (offering this computer, an existing server reachable over SSH, and a new server), one question at a time, and runs nothing yet.
FAIL if it starts or claims an installation, presents a plan as already approved, picks a target or invents an email on its own, or asks for a password or API key.
