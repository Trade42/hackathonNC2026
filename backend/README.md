# backend

Rust workspace for ferry-core (Axum API, WebSocket stream, payments, tracking, Gemini agent), with SQLx migrations for schema `ferry` in `backend/migrations`. See spec sections 2 to 5 in docs/PROMPT-backend.md.

## Layout

| Path | What |
|---|---|
| `Cargo.toml` | the workspace (`members = ["ferry-core"]`, edition 2024, small debug info) |
| `migrations/` | SQLx migrations, `<version>_<description>.sql`, embedded and run at startup |
| `ferry-core/src/main.rs` | startup: config, database, migrations, storage buckets, serve |
| `ferry-core/src/{config,db,storage,state,app,health}.rs` | config from the root `.env`, the pool and migrations, Supabase Storage buckets, `AppState`, the router with CORS, tracing and compression, `GET /healthz` |
| `ferry-core/src/{error,extract,jwt,ids,phone}.rs` | shared building blocks for every endpoint (below) |
| `ferry-core/src/<area>/mod.rs` | one module per area: auth, profile, catalog, runs, orders, payments, tracking, circles, invites, notifications, impact, agent, messaging, demo. Each exposes `router()`, already merged into the app. |
| `ferry-core/tests/` | integration tests; `tests/common/mod.rs` is the guard every file includes |

## Writing an endpoint

- Put it in your area's module: add routes to its `router()` with the full path from contract/openapi.yaml (`.route("/v1/runs/{id}/close", post(close))`). Add files under `src/<area>/`. You never need to touch `app.rs`.
- Return `ApiResult<T>` (`error::ApiError`): `ApiError::conflict(..)`, `::window_closed(..)`, `::insufficient_funds(..)` and the rest render the contract's `{"error": {"code", "message", "details"?}}` with the status in docs/DECISIONS.md, Contract. `validation_failed` is 400. `ApiError::internal(err)` logs and hides the cause. `?` on a `sqlx::Error` maps `RowNotFound` to 404 and an unreachable database to 503.
- Take input with `extract::Json`, `extract::Path` and `extract::Query`, not axum's own, so a bad body or parameter renders the envelope. `Option<extract::Json<T>>` accepts a missing body.
- Add `jwt::AuthUser` to a handler's arguments to require `Authorization: Bearer <jwt>`. It gives `user_id` or answers 401 `unauthorized`. Issue tokens with `state.jwt.issue_token(user_id)`, and verify a WebSocket `?token=` with `state.jwt.verify(token)`.
- New IDs: `ids::new_id(ids::ORDER)` gives `ord_` plus 12 random characters.
- Log phone numbers only through `phone::mask_phone` (`+1 ••• ••• 0102`, which is also the contract's `phone_masked` format for US numbers). Never log a secret. `config::Secret` values print as `Secret(..)`.
- `State<AppState>` holds the pool, the config, one `reqwest::Client` (5 s connect and 10 s total timeouts) and the JWT keys. A handler can also take one field, such as `State<PgPool>`.
- Schema-qualify every query (`ferry.orders`), even though connections use `search_path = ferry, public`.

## Build, test, run

You need cargo and the local Postgres 17 at 127.0.0.1:55432 (`/home/sal/hackNCprograms/tools/pg17.sh status`). From `backend/`:

```sh
cargo build
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

To run it in a worktree, use your own database and port, never the master `postgres` database (ferry-core refuses it) or ports 8080 and 8787. Create the database once (for example `CREATE DATABASE ferry_dev_<task>` in a small node `pg` script), then:

```sh
PORT=8110 DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55432/ferry_dev_<task> SUPABASE_SECRET_KEY= cargo run
curl -si localhost:8110/healthz
```

`SUPABASE_SECRET_KEY=` keeps a worktree run away from the real Storage project, because the root `.env` holds the real key. The log then shows `storage buckets skipped`. Stop the server by its PID.

### The shared dev server (main checkout only)

The shared dev server runs on 8080 from the main checkout, behind the tunnel, against Supabase. It runs the built binary directly, outside cargo, so `.cargo/config.toml` does not apply and `DATABASE_URL` comes from the root `.env` (the Supabase session pooler):

```sh
cd /home/sal/hackNCprograms/repo/backend && cargo build && ./target/debug/ferry-core
```

Run it from inside the repo (any directory below the root that holds `.env` and `contract/`), or set `FERRY_ENV_FILE` to the `.env` path.

## Configuration

ferry-core reads one root `.env`: the file `FERRY_ENV_FILE` names, or else the `.env` in the first directory at or above the current one that also holds `contract/`. The process environment wins over the file, so a run can override `PORT`, `DATABASE_URL` and the rest. An empty value means unset. Only `DATABASE_URL` is required. Every name is in `.env.example`. Defaults: `PORT` 8080, `NESSIE_MODE` mock, `DEMO_MODE` false, `TAX_RATE_BPS` 750, `FERRY_FEE_CENTS` 150. When `JWT_SECRET` is unset, ferry-core signs with a random per-process secret and warns. The startup log names each variable as set or unset, never its value. Set `RUST_LOG` to change the log level (default `info`, with Postgres notices at `warn`).

At startup ferry-core:
1. refuses a `DATABASE_URL` on port 6543 (the transaction pooler) or on the local master database;
2. runs `CREATE SCHEMA IF NOT EXISTS ferry` and the embedded migrations; `_sqlx_migrations` lands in `ferry`;
3. creates the `receipts` and `media` Storage buckets if they are missing, or skips with one warning when `SUPABASE_SECRET_KEY` is unset;
4. serves `GET /healthz`: 200 `{"status":"ok","db":"ok"}`, or 503 with `"db":"unreachable"` when `SELECT 1` does not answer within 2 s.

## Tests never reach Supabase

Tests run only on the local Postgres 17 at 127.0.0.1:55432. Two layers make sure of it:

1. **The guard in `ferry-core/tests/common/mod.rs`.** A constructor runs before the test harness and sets `DATABASE_URL` to `TEST_DATABASE_URL` from the process environment (not the `.env` file) when that is local, otherwise to `postgres://postgres:postgres@127.0.0.1:55432/postgres`. It also sets `SUPABASE_URL` and `SUPABASE_SECRET_KEY` to empty (unset), because `#[sqlx::test]` loads the root `.env` into the test process. It panics if `DATABASE_URL` is not local afterwards. It works from any directory and with `--manifest-path`. The unit-test binary runs the same guard from `src/lib.rs`.
   **Every integration test file must start with `mod common;`.** `tests/guard.rs` fails if one does not.
2. **`.cargo/config.toml` at the repo root** sets `DATABASE_URL` to the local Postgres for every process cargo starts from inside the repo (`cargo test`, `cargo run`), unless the process environment already sets it.

Tests that need Supabase Storage or another service build their `Config` explicitly (`common::config(&[...])`) and point it at a local stub server, as `tests/storage.rs` does.
