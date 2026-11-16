# Code Style

> Note: Since the UI crate split, `crates/datatab_ui/src/ui/...` paths referenced
> below are compatibility shims (`pub use` re-exports) at the old module paths.
> The canonical homes are `datatab_components`, `datatab_ui_base`,
> `datatab_ui_document`, `datatab_ui_sidebar`, and `datatab_ui_windows`. See
> `ARCHITECTURE.md` for the layered crate map.

## Naming Conventions

| Element                   | Convention           | Examples                                    | References                                                      |
| ------------------------- | -------------------- | ------------------------------------------- | --------------------------------------------------------------- |
| Types (struct/enum/trait) | PascalCase           | `ConnectionProfile`, `DbKind`               | crates/datatab_core/src/connection/profile.rs                               |
| Functions/methods         | snake_case           | `prepare_connect_profile`, `refresh_schema` | crates/datatab_app/src/app_state/mod.rs, crates/datatab_ui/src/ui/views/workspace/mod.rs |
| Fields/locals             | snake_case           | `active_connection_id`, `pending_command`   | crates/datatab_app/src/app_state/mod.rs, crates/datatab_ui/src/ui/views/workspace/mod.rs |
| Constants                 | UPPER_SNAKE_CASE     | `ROW_COMPACT`                               | crates/datatab_components/src/tokens.rs, crates/datatab_ui_base/src/keymap.rs |
| Tests                     | `test_` + snake_case | `test_parse_simple_key`                     | crates/datatab_app/src/keymap/chord.rs                               |

## File Organization

- The canonical repo layout, crate map, and key-file overview live in `ARCHITECTURE.md`.
- Workspace crates live under `crates/`. In practice, most changes land in `datatab_ui`, `datatab_app`, `datatab_core`, or one of the `datatab_driver_*` crates.
- Module directories use `mod.rs`.
- Keep new code in the existing crate and area that already owns the behavior instead of creating parallel structure.

## Import Style

- Imports are grouped at the top with braces for multi-item paths (crates/datatab_app/src/app_state/mod.rs, crates/datatab_ui/src/ui/views/workspace/mod.rs).
- Internal modules prefer `crate::` and `super::` paths for local imports (crates/datatab_ui/src/ui/views/workspace/mod.rs).
- External dependencies are listed separately from internal modules in the import block.

## Code Patterns

- GPUI entities: stateful UI pieces are `Entity<T>` values; updates use `entity.update(cx, |state, cx| { ... })` (crates/datatab_ui/src/ui/views/workspace/mod.rs).
- Background work: use `cx.background_executor().spawn(...)` for DB/IO, then `cx.spawn(...).update(...)` to re-enter UI thread (crates/datatab_ui/src/ui/views/workspace/mod.rs).
- Feature gating: drivers are compiled via `#[cfg(feature = "sqlite")]` / `#[cfg(feature = "postgres")]` (crates/datatab_app/src/app_state/mod.rs).
- External RPC drivers are registered with `rpc:<socket_id>` keys and use `DbConfig::External { kind, values }` for persisted profile config (crates/datatab_app/src/app_state/mod.rs, crates/datatab_core/src/connection/profile.rs).
- RPC protocol schemas and DTO conversions stay in `datatab_ipc`; transport/client logic stays in `datatab_driver_ipc` (crates/datatab_ipc/src/driver_protocol.rs, crates/datatab_driver_ipc/src/transport.rs).
- Default constructors and helpers use `new`/`default_*` naming (crates/datatab_core/src/connection/profile.rs, crates/datatab_core/src/query/types.rs).
- Results and metadata are plain structs/enums with small helper methods (crates/datatab_core/src/query/types.rs, crates/datatab_core/src/schema/types.rs).
- Generic store/manager: `JsonStore<T>` provides file persistence; type aliases (`ProfileStore`, `ProxyStore`) use named constructors (`.profiles()`, `.proxies()`). `ItemManager<T>` adds CRUD + auto-save; concrete managers are type aliases with `DefaultFilename` for `Default` impl (crates/datatab_core/src/storage/json_store.rs, crates/datatab_core/src/connection/item_manager.rs).
- Trait-based deduplication: `HasSecretRef` unifies keyring operations, `Identifiable` unifies ID access. Prefer a shared trait + generic method over per-type copy-paste (crates/datatab_core/src/storage/secret_manager.rs, crates/datatab_core/src/connection/item_manager.rs).
- Callback injection for cross-crate boundaries: `CreateTunnelFn` avoids circular dependency by defining a function signature in `datatab_core` and supplying the real implementation from the app crate (crates/datatab_core/src/connection/manager.rs, crates/datatab_app/src/proxy.rs).
- Reusable navigation components: `TreeNav` (plain struct, not Entity) for tree navigation; `FormGridNav<F>` for 2D grid form navigation. Both take dynamic state as input rather than storing it (crates/datatab_components/src/components/tree_nav/mod.rs, crates/datatab_ui_windows/src/settings/form_nav.rs).
- Shared process execution: process-backed hooks and `datatab.process.run()` should reuse `datatab_core::execute_streaming_process()` instead of maintaining separate polling or output-capture loops.
- Live script output: prefer a channel plus a document-owned buffer (`LiveOutputState`) for streamed UI output; do not use shared `Arc<Mutex<String>>` buffers for live rendering.
- Script languages (`Lua`, `Python`, `Bash`) are handled by `CodeDocument`; they execute as scripts, not DB queries, and should not depend on connection context UI.
- Driver-owned query generation: textual read/DML previews, copy-as-query flows, and MCP mutation/query previews should go through `Connection::query_generator()` instead of ad hoc UI-side SQL building. Keep `CodeGenerator` focused on DDL.
- Platform detection: use `platform::floating_window_kind()` for secondary windows (Settings, Connection Manager); use `platform::apply_window_options()` to set min size for X11 compatibility with tiling WMs.
- MCP governance: all MCP operations go through `McpGovernanceService` trait; policy decisions use `PolicyEngine::evaluate()` with `ExecutionClassification` and return `PolicyDecision`.
- Settings sections: implement `SettingsSection` trait for keyboard navigation; use `FormSection` trait for form-based sections with 2D grid navigation.
- Multi-select dropdowns: use `MultiSelect` component for multi-value selection; emits `MultiSelectChanged` on selection change.
- Value source selection: use `ValueSourceSelector` for fields that can be literal, env var, secret, parameter, or auth session field.

## Error Handling

- Domain errors use `DbError` and `Result<T, DbError>` (crates/datatab_core/src/core/error.rs, crates/datatab_core/src/core/traits.rs).
- App-level operations log failures and continue with fallback state (crates/datatab_app/src/app_state/mod.rs).
- Avoid panics; use `?`, `map_err`, and logged errors. Panics only appear in startup `expect` calls (crates/datatab/src/main.rs).
- Cancellation and unsupported features return explicit errors (crates/datatab_core/src/core/traits.rs).

## Logging

- Use `log::{info, warn, error, debug}` throughout app and driver layers (crates/datatab_app/src/app_state/mod.rs, crates/datatab_ui/src/ui/views/workspace/mod.rs).
- Logging is initialized via `env_logger` in the app entry point (crates/datatab/src/main.rs).

## Testing

- Unit tests live beside implementation in `#[cfg(test)] mod tests` blocks (crates/datatab_app/src/keymap/chord.rs).
- Tests use `#[test]` and `assert_eq!`/`assert!` with snake_case names.
- Integration tests for drivers use Docker containers managed by `datatab_test_support`; run ignored suites explicitly with `cargo test -p <crate> --test live_integration -- --ignored`.
- Test-only constructors (e.g., `ItemManager::with_store`) are gated behind `#[cfg(test)]`.
- For large GPUI-heavy modules, extract pure state helpers into small modules when that keeps unit tests simple and avoids bloating the main document module.

## Do's and Don'ts

- Do call `cx.notify()` after state changes that should re-render (crates/datatab_ui/src/ui/views/workspace/mod.rs).
- Do use `background_executor().spawn()` for DB operations to avoid blocking UI.
- Do propagate or log errors; do not silently discard fallible results.
- Do refactor and modularize logic functions that grow beyond ~100 lines (render functions that only build an element tree: ~200 lines); treat this as a design smell. See `CONTRIBUTING.md` § Pull Requests.
- Do use abstractions (`DatabaseCategory`, `QueryLanguage`, `DriverCapabilities`) to adapt UI behavior instead of driver-specific conditionals.
- Do use `QueryGenerator` for copied queries and textual read/DML previews; do not duplicate mutation or read template formatting in the UI.
- Do keep each `crates/datatab_driver_*/README.md` updated with current **Features** and **Limitations**.
- Do use `LuaCapabilities::all_enabled()` for editor-run Lua scripts so the script runner matches the full hook-testing environment.
- Do treat external service `Hello` metadata/form definition as the source of truth for RPC drivers.
- Do use `mod.rs` for module directories (e.g., `core/mod.rs`, not a sibling `core.rs`).
- Don't use deprecated GPUI types (`Model<T>`, `View<T>`, etc.).
- Don't add driver-specific logic in UI code (e.g., `if driver == "mongodb"`). Use capability flags and metadata from `DriverMetadata` instead.
- Don't import driver crates directly in UI code. All driver interaction goes through `datatab_core` traits.
- Don't route DML or read query templates through `CodeGenerator`; reserve `CodeGenerator` for DDL.
- Don't add a second subprocess execution path for hooks or Lua helpers when the shared streaming executor already fits the job.
- Don't use `config.json` to define driver metadata/forms for external services; it is runtime launch/socket config only.
- Do use type-erased handles (`Box<dyn Any + Send + Sync>`) when storing cross-crate RAII objects to avoid circular dependencies.
- Do use the `TunnelConnector` trait for new tunnel protocols instead of duplicating RAII/lifecycle logic.
- Don't combine proxy and SSH tunnel on the same connection (mutually exclusive, enforced in `ConnectProfileParams::execute()`).
- Do use `ExecutionClassification` to categorize operations for MCP policy decisions (Metadata/Read/Write/Destructive/AdminSafe/Admin/AdminDestructive).
- Do implement `SettingsSection` trait for settings sections that need keyboard navigation and dirty-state tracking.
- Do use `FormSection` trait for form-based settings sections with 2D grid navigation and blur handling.
- Do use `McpGovernanceService` trait for MCP-related governance operations; do not bypass policy engine.
- Do use `platform::floating_window_kind()` for secondary windows to avoid X11 transient dialog issues.
- Do log all MCP policy decisions via `AuditService` for compliance and debugging.
- Do emit audit events through `EventSink` trait rather than calling `AuditService` directly in service layers; this decouples services from the storage implementation.
- Do use canonical action string constants from `datatab_core::observability::actions` instead of bare string literals in audit events.
- Do set category-specific required fields before calling `EventSink::record()` — validation runs at record time and returns an error if fields are missing.
- Do not store full query text in `details_json` — the `AuditService` replaces it with a SHA256 fingerprint by default; if full text is needed, opt in explicitly with `set_capture_query_text(true)` and understand the compliance implications.
- Do use `pending_toast` pattern for toast notifications in sync event handlers: add `pending_toast: Option<PendingToast>` field, set it during async operations, flush in `render()` via `flush_pending_toast()`. This avoids the `window` parameter problem in non-async contexts.
- Do use dropdown menus for toolbar actions with `.on_mouse_down_out(cx.listener(...))` to close on outside click. Use toggle logic (clicking one closes the other) for mutually exclusive dropdowns. Use `AppIcon::CircleCheck` for active/selected states.
