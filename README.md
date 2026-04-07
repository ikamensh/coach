# Coach

Desktop app that monitors Claude Code sessions via hooks. Switches between **present** (manual control) and **away** (auto-approve, inject priorities) modes per session.

## Install

Download the latest release for your platform from [**Releases**](https://github.com/ikamensh/coach/releases/latest):

| Platform | File |
|---|---|
| macOS (Apple Silicon) | `Coach_*_aarch64.dmg` |
| macOS (Intel) | `Coach_*_x64.dmg` |
| Windows | `Coach_*_x64-setup.exe` |
| Linux (Debian/Ubuntu) | `Coach_*_amd64.deb` |
| Linux (other) | `Coach_*_amd64.AppImage` |

On macOS, open the `.dmg` and drag Coach to Applications. On first launch you may need to right-click → Open to bypass Gatekeeper.

## Hook Setup

Coach listens on `localhost:7700` (configurable) for integrations below.

### Claude Code

Click **Hooks** in the app header to install HTTP hooks into `~/.claude/settings.json`, or add manually:

```json
{
  "hooks": {
    "PermissionRequest": [{ "hooks": [{ "type": "http", "url": "http://localhost:7700/hook/permission-request" }] }],
    "Stop":              [{ "hooks": [{ "type": "http", "url": "http://localhost:7700/hook/stop" }] }],
    "PostToolUse":       [{ "hooks": [{ "type": "http", "url": "http://localhost:7700/hook/post-tool-use" }] }]
  }
}
```

### Cursor Agent (CLI)

Cursor uses `~/.cursor/hooks.json` with `command` hooks that receive JSON on stdin. Coach can install `curl` forwarders to separate HTTP routes under `/cursor/hook/...` (same port). Use **Install Cursor hooks** in the Hooks pane, or `coach hooks cursor install`.

## Settings

Persisted at `~/.coach/settings.json` — API tokens, model selection, priorities, theme.

## Development

```bash
npm install
npm run tauri:dev       # dev mode with hot reload
npm run tauri:build     # release build
```

Requires Node.js 18+ and Rust toolchain (`rustup`).

## Tests

```bash
cd src-tauri
cargo test                    # HTTP-level integration tests
cargo test -- --ignored       # also runs real claude CLI test
```

## Releasing

Push a version tag to trigger cross-platform builds:

```bash
git tag v0.1.0
git push origin v0.1.0
```

GitHub Actions builds for macOS (ARM + Intel), Windows, and Linux, then uploads all installers to a new GitHub Release automatically.
