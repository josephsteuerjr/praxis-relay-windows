# Ouroboros patch for Praxis Relay

Windows PowerShell patcher for the released Ouroboros 6.64.0 one-folder build. It adds one dedicated Praxis Relay preset beside the existing generic OpenAI-compatible provider; the generic provider is not replaced.

The repository history is preserved. The former relay source was replaced by this focused patch package.

## What the patch adds

- Base URL: http://localhost:5011
- API key: auto
- Provider route: openai-compatible
- A Praxis Relay button in onboarding and Settings
- Relay models in every relevant selector
- Relay-aware model discovery through GET /v1/models
- Relay-aware Capability Evidence metadata lookup through GET /v1/models
- The existing real generative capability check remains active through POST /chat/completions when the catalog does not publish a context window

Selector models:

- gpt-5.6-sol
- gpt-5.6-terra
- gpt-5.6-luna
- gpt-5.5
- gpt-5.4
- gpt-5.4-mini

The default Main, Heavy, Vision, and Consciousness model is gpt-5.6-terra. Light and Fallback use gpt-5.4-mini. Pass -Model to choose a different main relay model.

## Requirements

- Extracted official Windows release Ouroboros 6.64.0
- GitHub Desktop installed
- Windows PowerShell 5.1
- Ouroboros stopped during patching

The script uses GitHub Desktop's bundled Git CLI directly. It never opens the GitHub Desktop GUI.

## Run

From Windows PowerShell:

    powershell.exe -ExecutionPolicy Bypass -File .\patch-ouroboros-relay.ps1 -ReleasePath "C:\path\to\Ouroboros"

If the release folder is next to this repository and is named Ouroboros, -ReleasePath can be omitted.

To select another main model:

    powershell.exe -ExecutionPolicy Bypass -File .\patch-ouroboros-relay.ps1 -ReleasePath "C:\path\to\Ouroboros" -Model gpt-5.6-sol

To patch code without changing the current settings.json:

    powershell.exe -ExecutionPolicy Bypass -File .\patch-ouroboros-relay.ps1 -ReleasePath "C:\path\to\Ouroboros" -SkipSettings

Start Praxis Relay first, then launch Ouroboros.exe.

## Commit and startup mechanics

The patch is distributed as a Git mail patch. The script uses git am, so a successful first run always creates a real commit in the launcher-managed Ouroboros repository. It then verifies:

1. the released bundle and patch SHA-256 hashes;
2. a clean managed worktree;
3. ancestry from the exact v6.64.0 source commit;
4. creation of a new commit containing the patch marker;
5. Ouroboros's own restart gate: unmerged-index check, py_compile server.py, and imports of the core boot surface;
6. the targeted Python test gate using the release's embedded interpreter;
7. a clean worktree after the commit;
8. OpenAI-compatible routing to localhost:5011 with key auto;
9. invalidation of repo/**/__pycache__ and data/state/pycache before the bootstrap pin is cleared.

Only after those checks does it clear Ouroboros's one-shot bootstrap pin. A repeated run is idempotent: it verifies the existing marker commit and does not create an empty commit.

If the managed branch is still on an older clean release ancestor, the script fast-forwards it to the exact 6.64.0 source before applying the commit.

## Rollback

Before changing Git history, the script creates a local rollback branch named ouroboros-patch-backup/date-time-oldhead. If applying or testing fails, it automatically restores the exact clean starting HEAD.

Before editing existing settings, it creates a timestamped settings.json.praxis-relay.*.bak copy and preserves every unrelated setting.

## Scope and integrity

- Target app version: 6.64.0
- Target source commit: ffcd09770438f2ebf78b3ec775ec23084e66994b
- Patch SHA-256: 7207e4a6cdd23b698df70fe98074461d227cfd6c026f747552a5c417cfecc027

The patch intentionally fails closed on other Ouroboros releases or a divergent managed branch. No relay credentials, account data, or relay binaries are included.

## License

MIT. See LICENSE.
