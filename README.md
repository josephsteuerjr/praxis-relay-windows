# Praxis Relay for Windows

`relay` is a standalone Rust/Axum service that exposes a small OpenAI-compatible API on
`127.0.0.1:5011` for a local Praxis deployment.

This Windows-focused edition adds the Praxis model contract, multimodal input, hosted web
search mapping, usage-limit reporting, a native Windows build, and a relay-owned OAuth store
that stays isolated from the user's normal Codex credentials.

## Upstream and license

This project is derived from
[unluckyjori/Codex-Proxy-Server](https://github.com/unluckyjori/Codex-Proxy-Server)
at upstream revision `57417d107dc100d4dfd15fd3fcf11350e9b71088`.
The original project and this derivative are distributed under the MIT License. The original
copyright and permission notice are preserved in [LICENSE](LICENSE).

This project is independently maintained and is not affiliated with or endorsed by OpenAI.

It exposes:

- `POST /chat/completions`
- `GET /v1/models`
- `GET /v1/limits`
- `GET /health`

## Run locally

Install a current Rust toolchain, then run:

```bash
cd relay
cargo run
```

Choose `1` in the menu to start the server. The Dockerfile provides a container build for
Linux deployments; the process deliberately binds only to loopback, so expose it through a
separate reverse proxy only when that is an explicit deployment decision.

### Windows

Build and run the native executable from PowerShell:

```powershell
cargo build --release --locked
Copy-Item .\target\release\codex-proxy-server.exe .\praxis-relay.exe
.\praxis-relay.exe
```

The server itself does not require Python. Menu option `3` (interactive ChatGPT login) uses
the first working Python 3 launcher among `python.exe`, `py.exe -3`, and `python3.exe`.
Set `RELAY_PYTHON` to an explicit interpreter path if automatic discovery is unsuitable.

### Separate relay authentication

The relay never reads `~/.codex/auth.json`, `~/.opencode/auth.json`, or the generic
`OPENAI_API_KEY` environment variable. Its credentials live only in `local_auth/auth.json`
next to the executable, matching the isolated `/app/local_auth` mount used on the server.

On a clean installation, start the executable and choose menu option `3` to authorize the
relay account. Then choose `1` to serve the API. To place credentials elsewhere, set
`RELAY_AUTH_DIR` to an explicit directory before starting the relay.

## Authentication and privacy

The relay reads authentication only from its dedicated auth directory. Never commit
`auth.json`, API keys, session files, or relay logs. This public copy contains source code
and test fixtures only; it deliberately contains no account data.
