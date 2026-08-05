# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

### Development
```
npm run tauri dev        # Start Tauri app in dev mode (launches Vite + Rust watcher)
npm run dev              # Start Vite frontend only (no Tauri shell)
```

### Building
```
npm run tauri build      # Full production build (frontend + Rust binary + installer)
npm run build            # TypeScript check + Vite bundle only
```

### Type checking & linting
```
npx tsc --noEmit                    # TypeScript type check
cd src-tauri && cargo clippy        # Rust linting
cd src-tauri && cargo check         # Fast Rust type check (no codegen)
```

### Testing
```
cd src-tauri && cargo test          # Run Rust tests
```

## Architecture

This is a **Tauri v2** desktop app — React/TypeScript frontend compiled by Vite, Rust backend compiled by Cargo, bridged by Tauri's IPC layer.

### IPC bridge (the core pattern)
- Rust commands are annotated with `#[tauri::command]` and defined in `src-tauri/src/lib.rs`
- They are registered in `lib.rs::run()` via `tauri::generate_handler![cmd1, cmd2, ...]`
- The frontend calls them with `invoke("command_name", { argName: value })` from `@tauri-apps/api/core`
- `main.rs` is a thin shim; all logic lives in `lib.rs` (required for the `rlib`/`cdylib` dual-crate setup on Windows)

### Permissions / capabilities
- `src-tauri/capabilities/default.json` controls what the frontend window is allowed to do (filesystem access, shell access, etc.)
- Tauri v2 uses an allowlist model: capabilities must be explicitly granted before plugins work

### Dev server
- Vite is fixed to port **1420** (`strictPort: true` in `vite.config.ts`) — Tauri's `devUrl` in `tauri.conf.json` points there
- `beforeDevCommand` in `tauri.conf.json` auto-starts `npm run dev` when you run `npm run tauri dev`
- The `src-tauri/` directory is excluded from Vite's file watcher to prevent spurious reloads

### Key config files
| File | Purpose |
|------|---------|
| `src-tauri/tauri.conf.json` | App metadata, window config, bundle targets |
| `src-tauri/Cargo.toml` | Rust dependencies and crate config |
| `src-tauri/capabilities/default.json` | Runtime permission grants for the main window |
| `vite.config.ts` | Frontend bundler config (Tauri-specific overrides) |

## Architecture Decisions

### Accessibility-tree automation
Uses the **Terminator** crate as a native Rust dependency, not its Node.js bindings. Because the backend is already Rust, pulling in Terminator directly avoids an unnecessary FFI/bridging layer.

### AI model stack
Models are routed per task to balance latency, cost, and capability:

| Task | Model | Deployment |
|------|-------|------------|
| Ghost Mode labeling | Qwen2.5-0.5B (fallback: Qwen2.5-1.5B) | Local |
| Vision fallback / drift-repair grounding | Qwen3.5-0.8B-Vision | Local |
| Chat Mode intent parsing + form Q&A generation | Qwen3-7B-Instruct | Cloud (Fireworks AI) |
| Heavy UI / vision / OCR tasks | Qwen2-VL-7B | Cloud (Fireworks AI) |

### SQLite schema additions
Two tables live in the main SQLite store beyond the original schema:

- **`contacts_cache`** — used for Chat Mode's email/contact resolution
- **`profile_fields` / `form_field_mappings`** — used for Form Memory's dropdown fuzzy-matching and field-mapping logic
