<!--
Read the docs before submitting: https://miguelenes.github.io/ollama-router/
Never include prompts, embeddings, tokens, share tokens, SSH keys, or
RUNPOD_API_KEY in this PR.
-->

## What

<!-- What does this PR change and why? -->

## Honest-fleet check

- [ ] Keeps list = union / infer = holders-only / pull = placement / miss = 503 `model_missing`
- [ ] No Thunder, no public tunnels as healthy, no fleet.yaml writes from enroll
- [ ] Streams are forwarded as they arrive; retries only pre-first-byte

## Verification

- [ ] `task check` green
- [ ] `task coverage` ≥ 80% (Rust/test changes)
- [ ] Docs updated when behavior changed (site + README where relevant)
