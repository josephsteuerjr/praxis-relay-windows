# Ouroboros 6.64.0 / Praxis Relay integration specification

Status: implemented

Integration target: Ouroboros 6.64.0 for Windows

Relay release: Praxis Relay v0.2.0

Patch marker: `.ouroboros-patches/praxis-relay-v3.json`

This document defines the observable contract of the Windows patch package. The words
MUST, MUST NOT, SHOULD, and MAY describe required or optional behavior of a conforming
installation.

## 1. Purpose and scope

The integration adds one dedicated Praxis Relay preset to the existing Ouroboros
OpenAI-compatible provider. It does not replace or globally alter the generic provider.

The package consists of:

- `patch-ouroboros-relay.ps1`, the Windows installer and verification transaction;
- `patches/ouroboros-v6.64.0-praxis-relay.patch`, one Git mail patch for Ouroboros;
- the native Praxis Relay executable published as a GitHub Release asset;
- relay source on the `relay-source-v0.2.0` branch.

The relay and Ouroboros remain separate processes. Ouroboros is the client; Praxis Relay
owns upstream authentication and translates the supported OpenAI-compatible request into
the upstream API contract.

## 2. Compatibility boundary

The installer MUST verify all of the following before modifying the managed repository:

- the released application version is `6.64.0`;
- the embedded release source is commit
  `ffcd09770438f2ebf78b3ec775ec23084e66994b`;
- the embedded `repo.bundle` and the unified mail patch match their pinned SHA-256 values;
- the managed Ouroboros worktree is clean, including untracked files;
- `HEAD` is attached to a branch;
- the current branch is on the 6.64.0 release lineage.

The supported Git states are:

| Starting state | Required behavior |
| --- | --- |
| An older clean ancestor of 6.64.0 | Fast-forward to the exact 6.64.0 source, then apply the patch |
| The exact 6.64.0 source | Apply the patch |
| A clean descendant of 6.64.0 with existing commits | Preserve those commits and apply the patch on top |
| A divergent or unrelated branch | Refuse without applying the patch |
| A dirty worktree or detached `HEAD` | Refuse without changing history |

Existing commits MUST NOT be squashed, rebased, or rewritten. A successful first
installation MUST create exactly one new patch commit after the selected pre-patch `HEAD`. A
fast-forward from an older release may expose existing upstream commits, but it does not
count as a commit created by the installer.

## 3. Relay API contract

Ouroboros MUST use the following literal preset:

| Field | Value |
| --- | --- |
| Base URL | `http://localhost:8088` |
| API key | `auto` |
| Provider | `openai-compatible` |
| Chat Completions | `POST /chat/completions` |
| Model catalog | `GET /v1/models` |
| Context window | `1050000` |
| Billing mode | `subscription` |

The Chat Completions path deliberately has no `/v1` prefix. Model discovery deliberately
does. A conforming client MUST NOT normalize the two paths into the same prefix.

Praxis Relay binds to loopback on port 8088. `auto` is a local preset sentinel, not an
upstream API key and not a value the user must type into the generic provider form. The
relay obtains upstream authorization from its isolated `local_auth/auth.json` store or the
explicit `RELAY_AUTH_DIR`; it MUST NOT depend on the user's normal Codex credentials.

The relay additionally exposes `GET /v1/limits` and `GET /health` for operator use. These
endpoints are not required for Ouroboros model selection.

### 3.1 Chat Completions behavior

The relay MUST accept OpenAI-compatible message and tool envelopes used by Ouroboros.

- `stream=true` returns an OpenAI-compatible server-sent event stream.
- `stream=false` returns one normal `chat.completion` object.
- A non-stream response MUST preserve either assistant text or `tool_calls` and the matching
  finish reason.
- An omitted function-tool `strict` flag is interpreted as `false`.
- An explicitly supplied `strict` value is preserved.
- Hosted web-search events are relay-owned and MUST NOT be exposed as client function calls.

## 4. Model contract

`GET /v1/models` and every patched Ouroboros selector MUST expose the same six model IDs:

1. `gpt-5.6-sol`
2. `gpt-5.6-terra`
3. `gpt-5.6-luna`
4. `gpt-5.5`
5. `gpt-5.4`
6. `gpt-5.4-mini`

Every catalog entry MUST advertise `context_window: 1050000`.

The default patched model slots are:

| Ouroboros slot | Default model |
| --- | --- |
| Main | `openai-compatible::gpt-5.6-terra` |
| Heavy | `openai-compatible::gpt-5.6-terra` |
| Light | `openai-compatible::gpt-5.4-mini` |
| Vision | `openai-compatible::gpt-5.6-terra` |
| Consciousness | `openai-compatible::gpt-5.6-terra` |
| Fallback | `openai-compatible::gpt-5.4-mini` |

`-Model` MAY select another model from the six-item list for Main, Heavy, Vision, and
Consciousness. Light and Fallback remain `gpt-5.4-mini`.

Capability Evidence and the Ouroboros model catalog MUST query `/v1/models` for this exact
preset. Other OpenAI-compatible endpoints retain their original `/models` discovery path.

## 5. Settings contract

Unless `-SkipSettings` is supplied, the installer writes these managed values to
`data/settings.json`:

- `OPENAI_COMPATIBLE_BASE_URL=http://localhost:8088`
- `OPENAI_COMPATIBLE_API_KEY=auto`
- the six Ouroboros model-slot values described above;
- all corresponding `USE_LOCAL_*` switches set to `false`.

Every unrelated settings property MUST be preserved. If `settings.json` already exists,
the installer MUST create a timestamped
`settings.json.praxis-relay.<timestamp>.bak` before replacing it atomically.

The exact pair `http://localhost:8088` and `auto` identifies the subscription-backed route.
Only this pair bypasses token-price admission by using a zero dollar budget. Unknown pricing
on other generic OpenAI-compatible endpoints remains fail-closed.

## 6. Patch transaction

Before modifying history, the installer creates
`ouroboros-patch-backup/<timestamp>-<old-head>` at the exact starting commit.

It then MUST:

1. fetch and verify the pinned release commit from the embedded bundle;
2. fast-forward only when the current branch is an older release ancestor;
3. apply the unified patch with `git am --3way`;
4. verify that exactly one commit was created and that it contains the v3 marker;
5. run the release bootstrap compile/import gate with the embedded Python runtime;
6. run the targeted Ouroboros integration tests;
7. update and verify settings unless `-SkipSettings` was requested;
8. remove repository and runtime Python bytecode caches;
9. clear the one-shot bootstrap pin only after every required gate succeeds;
10. finish with a clean worktree.

If patch application or a required gate fails, the installer MUST abort an active `git am`,
restore the exact starting `HEAD`, and restore settings changed by that installation
transaction. The rollback branch remains available for inspection.

## 7. Repeated invocation

The v3 marker is the authoritative installed-state marker. Earlier marker files are retained
as patch history but are not used for current installer validation.

On a repeated invocation, the installer MUST verify the marker fields and locate the commit
that introduced the marker. That commit's parent MUST be either the exact 6.64.0 source or
a descendant of it. This permits installations that already had valid post-6.64.0 commits
while still rejecting markers from unrelated history.

A valid repeated invocation MUST NOT create an empty or duplicate commit. It MAY refresh the
managed settings and route check unless `-SkipSettings` is supplied.

## 8. Security and non-goals

- The relay MUST listen on loopback unless an operator deliberately places a separate reverse
  proxy in front of it.
- Credentials, account data, session files, and logs MUST NOT be committed to this repository.
- The installer MUST NOT start GitHub Desktop's GUI; it uses only the bundled Git executable.
- The integration does not add a general remote-relay configuration surface.
- The integration does not change the behavior of unrelated Ouroboros providers.
- The project is independently maintained and is not affiliated with or endorsed by OpenAI.

## 9. Conformance result

An installation is conforming only when:

- the managed repository ends clean with one new unified patch commit;
- the v3 marker matches the relay contract;
- the patched compile/import and targeted test gates pass;
- the selected model resolves through `openai-compatible` to
  `http://localhost:8088/chat/completions`;
- `/v1/models` supplies the six specified model IDs with the required context window;
- a repeated installer run recognizes the existing commit without adding another one.
