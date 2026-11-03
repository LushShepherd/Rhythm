# Contributing to Rhythm

Rhythm uses one shared Rust workspace and one web interface. The native desktop app currently runs on **Windows only**. macOS and Linux support is the next platform work; passing core tests on those systems does not mean their native app is implemented.

The project is licensed under the [MIT License](LICENSE).

## Set up a checkout

Install Git, [Rust through rustup](https://rustup.rs/), and Node.js 22 or newer. `rust-toolchain.toml` selects Rust **1.99.1**, including rustfmt and Clippy. Keep `Cargo.lock` and `apps/desktop/package-lock.json` in version control.

Use the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for Windows C++ Build Tools and WebView2. On macOS, install the Xcode command-line tools for Rust dependencies that compile native code. On Linux, install C/C++ build tools, `pkg-config`, and OpenSSL development headers for the HTTP client's native TLS support. SQLite is bundled with the Rust dependency.

From the repository root:

```sh
npm ci --prefix apps/desktop --ignore-scripts
cargo test --workspace --locked
npm test --prefix apps/desktop
```

No journal database, Google registration, or `.tools` directory is needed for these checks. On Windows, `scripts/cargo.ps1` can use an optional local toolchain under `.tools`; otherwise it uses Cargo from the system PATH.

## Preview and build

The interface preview works on Windows, macOS, and Linux:

```sh
node scripts/preview.mjs
```

Open <http://localhost:8767>. The preview has separate browser storage; it does not connect to the Windows journal or provide native lifecycle handling. Stop it with Ctrl+C.

For the native Windows app, run these from the repository root in PowerShell:

```powershell
.\scripts\cargo.ps1 run -p rhythm-desktop --locked
.\scripts\desktop.ps1 build --ci --bundles nsis '--' --locked
```

The installer is written to `target/release/bundle/nsis/`. The packaging helper also bundles the machine-control CLI. Quote `'--'` when passing Cargo arguments through a PowerShell helper.

## Optional Google Drive configuration

A normal checkout builds without Google registration; the app then reports that automatic sync is unavailable in that build. To make a configured Windows build, create your own Google **Desktop app** OAuth client and follow [the automatic-sync setup guide](docs/automatic-sync.md).

Keep its downloaded JSON outside the repository and supply its local path only to the build:

```powershell
$env:RHYTHM_GOOGLE_OAUTH_CLIENT_FILE = 'C:\private\rhythm-google-desktop.json'
.\scripts\desktop.ps1 build --ci --bundles nsis '--' --locked
```

The build embeds the desktop application registration. User access tokens, refresh tokens, passphrases, and encryption keys are never build inputs. Do not commit these, the downloaded registration, personal journals, backups, exports, or test profiles. Use synthetic notes and isolated profiles for sync testing.

## Source map

| Location | Responsibility |
| --- | --- |
| `crates/rhythm-domain` | Timer states, commands, and timing rules |
| `crates/rhythm-application` | Application operations over storage and clock interfaces |
| `crates/rhythm-storage` | SQLite migrations, notes, history queries, backups, and portable sync records |
| `crates/rhythm-desktop-service` | Serialized desktop operations and views shared by the UI and CLI |
| `crates/rhythm-sync` | Google OAuth, Drive transport, and encrypted objects |
| `crates/rhythm-platform-windows` | Windows clocks and power events; limited non-Windows fallback for core development |
| `crates/rhythm-control`, `crates/rhythm-cli` | Local desktop control protocol and command-line tools |
| `apps/desktop/src-tauri` | Windows native shell, sync orchestration, notifications, file dialogs, and credential protection |
| `apps/desktop/ui` | Shared interface, browser preview, translations, and frontend tests |
| `docs`, `prototypes` | Current guides and verification records; historical design exploration |

## Before submitting changes

Run the checks appropriate to the change. The complete local check sequence is:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
npm test --prefix apps/desktop
```

The GitHub Actions matrix checks the shared core on Windows, macOS, and Linux and includes native Windows code on its Windows runner. Device tests remain separate evidence. Describe the tested behavior, failures, and any remaining platform limitations in your change; update the relevant current guide when behavior changes. Prefer focused regression tests for data integrity, timing, network recovery, and asynchronous UI behavior.

## Starting the macOS and Linux ports

Keep the domain, storage format, encryption, and frontend shared. Start the native shell on the target OS, then supply platform implementations for clocks and sleep/wake events, notifications, secure credential storage, browser authorization, file dialogs, local IPC, and packaging. Windows DPAPI-protected credentials cannot be copied to another OS; that installation must authorize Google and unlock its sync collection itself.

The existing desktop entry point and dependencies are Windows-gated. The portable fallback clock is for development, not a validated replacement for native lifecycle handling. Preserve the current policy: interruption ends the active period at its last verified awake boundary, keeps its writing, and returns to untimed Break.

Validate installation, note saving, notifications, sleep/wake, input methods, and sync on each actual platform. Use the Windows app as the other participant in real cross-platform sync checks. Keep Windows regression checks running while adding each platform, and record unfinished device checks in [the roadmap](docs/roadmap.md).
