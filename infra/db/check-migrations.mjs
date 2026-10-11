#!/usr/bin/env node
// Applies backend/migrations/*.sql to throwaway databases on a LOCAL Postgres, in version order and
// each in its own transaction (as sqlx::migrate! does), and checks the ferry schema's invariants:
// RLS on every table, nothing for anon or authenticated, nothing Ferry-made in public, working views.
//
// It runs the migrations three times: with the roles anon and authenticated absent (plain Postgres),
// with them present (as on Supabase, including Supabase's default grants in public), and in ferry-core's
// real layout. The last starts like backend/ferry-core/src/db.rs with sqlx 0.9: search_path ferry,
// public, schema ferry first, sqlx's table in ferry, as a database owner that is neither superuser nor
// BYPASSRLS (like Supabase's postgres), then a second startup that must change nothing. Roles are
// cluster-wide, so it creates them only if missing and drops only what it created.
//
// Usage: node check-migrations.mjs [postgres-url]
// Without an argument it reads TEST_DATABASE_URL the way ferry-core loads the root .env: the process
// environment first, then the file named by FERRY_ENV_FILE, or else the .env in the first directory
// up from the current one that holds both .env and contract/. It refuses any host but 127.0.0.1 or localhost.
// It never prints the URL. Prints one PASS or FAIL line per check and exits 1 on any FAIL.

import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseEnv } from 'node:util';
import pg from 'pg';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const MIGRATIONS_DIR = path.resolve(HERE, '../../backend/migrations');
const DEFAULT_URL = 'postgres://postgres:postgres@127.0.0.1:55432/postgres';
const LOCAL_HOSTS = new Set(['127.0.0.1', 'localhost']);
const API_ROLES = ['anon', 'authenticated'];
const MIGRATOR_ROLE = 'ferry_check_migrator'; // owns the core-base-layout database; NOLOGIN, used via SET ROLE
const MARKED_ROLES = [...API_ROLES, MIGRATOR_ROLE];
const ROLE_MARK = 'ferry check-migrations temporary role';
const DB_PREFIX = 'ferry_check_';
const LOCK_KEY = '7202610100001'; // pg_advisory_lock key: one run of this script at a time per cluster

const results = [];
function record(ok, name, detail = '') {
  results.push(ok);
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? ` (${detail})` : ''}`);
}
function note(text) {
  console.log(`      ${text}`);
}
function quoteIdent(name) {
  return `"${name.replaceAll('"', '""')}"`;
}

// ---------- Where to connect ----------

function findEnvFile() {
  if (process.env.FERRY_ENV_FILE) return process.env.FERRY_ENV_FILE;
  for (let dir = process.cwd(); ; dir = path.dirname(dir)) {
    if (fs.existsSync(path.join(dir, '.env')) && fs.existsSync(path.join(dir, 'contract'))) {
      return path.join(dir, '.env');
    }
    if (path.dirname(dir) === dir) return null;
  }
}

function resolveUrl() {
  if (process.argv[2]) return { url: process.argv[2], source: 'command-line argument' };
  if (process.env.TEST_DATABASE_URL) return { url: process.env.TEST_DATABASE_URL, source: 'TEST_DATABASE_URL from the environment' };
  const file = findEnvFile();
  if (file) {
    const vars = parseEnv(fs.readFileSync(file, 'utf8'));
    if (vars.TEST_DATABASE_URL) return { url: vars.TEST_DATABASE_URL, source: `TEST_DATABASE_URL from ${file}` };
  }
  return { url: DEFAULT_URL, source: 'built-in default; TEST_DATABASE_URL is unset' };
}

// Builds the pg config from the parsed URL, so nothing else in the URL can redirect the connection.
function localConfig(raw) {
  let url;
  try {
    url = new URL(raw);
  } catch {
    throw new Error('the database URL does not parse');
  }
  if (url.protocol !== 'postgres:' && url.protocol !== 'postgresql:') {
    throw new Error(`refusing scheme ${url.protocol}; expected postgres:`);
  }
  if (!LOCAL_HOSTS.has(url.hostname)) {
    throw new Error(`refusing host ${url.hostname || '(none)'}: only 127.0.0.1 or localhost`);
  }
  if (url.searchParams.has('host') || url.searchParams.has('hostaddr')) {
    throw new Error('refusing a host or hostaddr query parameter');
  }
  return {
    host: url.hostname,
    port: Number(url.port || 5432),
    user: decodeURIComponent(url.username) || 'postgres',
    password: decodeURIComponent(url.password),
    database: decodeURIComponent(url.pathname.slice(1)) || 'postgres',
    application_name: 'ferry-check-migrations',
  };
}

// ---------- Migrations ----------

function loadMigrations() {
  const migrations = [];
  for (const file of fs.readdirSync(MIGRATIONS_DIR).sort()) {
    if (!file.endsWith('.sql') || file.endsWith('.down.sql')) continue;
    const match = /^(\d+)_(.+?)(\.up)?\.sql$/.exec(file);
    if (!match) throw new Error(`${file} is not named <version>_<description>.sql`);
    const sql = fs.readFileSync(path.join(MIGRATIONS_DIR, file), 'utf8');
    migrations.push({
      file,
      version: BigInt(match[1]),
      description: match[2].replaceAll('_', ' '),
      sql,
      checksum: createHash('sha384').update(sql).digest(),
      noTransaction: sql.startsWith('-- no-transaction'),
    });
  }
  migrations.sort((a, b) => (a.version < b.version ? -1 : a.version > b.version ? 1 : 0));
  for (let i = 1; i < migrations.length; i++) {
    if (migrations[i].version === migrations[i - 1].version) {
      throw new Error(`two migrations share version ${migrations[i].version}`);
    }
  }
  if (migrations.length === 0) throw new Error(`no migrations in ${MIGRATIONS_DIR}`);
  return migrations;
}

// sqlx's bookkeeping table, created unqualified before the first migration, as sqlx does: in public
// on a default search_path, in ferry under ferry-core's search_path = ferry, public.
const SQLX_TABLE = `CREATE TABLE IF NOT EXISTS _sqlx_migrations (
  version BIGINT PRIMARY KEY,
  description TEXT NOT NULL,
  installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),
  success BOOLEAN NOT NULL,
  checksum BYTEA NOT NULL,
  execution_time BIGINT NOT NULL
)`;

async function applyMigrations(client, migrations) {
  await client.query(SQLX_TABLE);
  for (const m of migrations) {
    const started = process.hrtime.bigint();
    try {
      if (!m.noTransaction) await client.query('BEGIN');
      // No parameters: the simple query protocol runs the whole file, DO blocks included.
      await client.query(m.sql);
      await client.query(
        'INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES ($1, $2, TRUE, $3, $4)',
        [m.version.toString(), m.description, m.checksum, (process.hrtime.bigint() - started).toString()],
      );
      if (!m.noTransaction) await client.query('COMMIT');
    } catch (err) {
      if (!m.noTransaction) await client.query('ROLLBACK').catch(() => {});
      throw new Error(`${m.file}: ${err.message}`);
    }
  }
}

// ferry-core's startup (backend/ferry-core/src/db.rs) with sqlx 0.9's Migrator::run: every statement
// that touches the schema or _sqlx_migrations (not sqlx's advisory lock). The connection sets
// search_path = ferry, public, and ferry-core creates schema ferry. sqlx creates its table unqualified
// (so in ferry), refuses a dirty database, and checks every applied version and checksum. It applies
// each new file in its own transaction, with its INSERT (execution_time -1), and sets execution_time
// after COMMIT. Returns the files it applied.
async function startLikeFerryCore(client, migrations) {
  await client.query('SET search_path = ferry, public');
  await client.query('CREATE SCHEMA IF NOT EXISTS ferry');
  await client.query(SQLX_TABLE);
  const dirty = await rows(client, 'SELECT version FROM _sqlx_migrations WHERE success = false ORDER BY version LIMIT 1');
  if (dirty.length) throw new Error(`migration ${dirty[0].version} is dirty`);
  const applied = new Map(
    (await rows(client, 'SELECT version, checksum FROM _sqlx_migrations ORDER BY version')).map((r) => [BigInt(r.version), r.checksum]),
  );
  for (const version of applied.keys()) {
    if (!migrations.some((m) => m.version === version)) throw new Error(`applied migration ${version} is not in backend/migrations`);
  }
  const ran = [];
  for (const m of migrations) {
    if (applied.has(m.version)) {
      if (!applied.get(m.version).equals(m.checksum)) throw new Error(`${m.file}: checksum differs from the applied one`);
      continue;
    }
    const started = process.hrtime.bigint();
    try {
      if (!m.noTransaction) await client.query('BEGIN');
      await client.query(m.sql);
      await client.query(
        'INSERT INTO _sqlx_migrations ( version, description, success, checksum, execution_time ) VALUES ( $1, $2, TRUE, $3, -1 )',
        [m.version.toString(), m.description, m.checksum],
      );
      if (!m.noTransaction) await client.query('COMMIT');
    } catch (err) {
      if (!m.noTransaction) await client.query('ROLLBACK').catch(() => {});
      throw new Error(`${m.file}: ${err.message}`);
    }
    await client.query('UPDATE _sqlx_migrations SET execution_time = $1 WHERE version = $2', [
      (process.hrtime.bigint() - started).toString(),
      m.version.toString(),
    ]);
    ran.push(m.file);
  }
  return ran;
}

// ---------- Checks ----------

async function rows(client, sql, params = []) {
  return (await client.query(sql, params)).rows;
}

async function publicObjects(client) {
  const found = await rows(client, `
    SELECT 'relation ' || c.relname || ' (relkind ' || c.relkind::text || ')' AS obj
      FROM pg_class c WHERE c.relnamespace = 'public'::regnamespace
    UNION ALL
    SELECT 'routine ' || p.oid::regprocedure::text FROM pg_proc p WHERE p.pronamespace = 'public'::regnamespace
    UNION ALL
    SELECT 'type ' || t.typname FROM pg_type t WHERE t.typnamespace = 'public'::regnamespace`);
  return new Set(found.map((r) => r.obj));
}

async function extensions(client) {
  return new Set((await rows(client, 'SELECT extname FROM pg_extension')).map((r) => r.extname));
}

async function checkRls(client, tag) {
  const tables = await rows(client, `
    SELECT c.relname, c.relrowsecurity, c.relforcerowsecurity
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'ferry' AND c.relkind IN ('r', 'p')
     ORDER BY 1`);
  const off = tables.filter((t) => !t.relrowsecurity).map((t) => t.relname);
  const forced = tables.filter((t) => t.relforcerowsecurity).map((t) => t.relname);
  const policies = await rows(client, "SELECT tablename FROM pg_policies WHERE schemaname = 'ferry'");
  record(
    tables.length > 0 && off.length === 0 && forced.length === 0 && policies.length === 0,
    `${tag} RLS enabled on every ferry table, not forced, no policies`,
    tables.length === 0
      ? 'no tables in schema ferry'
      : off.length || forced.length || policies.length
        ? `RLS off: ${off.join(', ') || 'none'}; forced: ${forced.join(', ') || 'none'}; policies on: ${policies.map((p) => p.tablename).join(', ') || 'none'}`
        : `${tables.length} tables`,
  );
}

async function checkPrimaryKeys(client, tag) {
  const missing = await rows(client, `
    SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'ferry' AND c.relkind IN ('r', 'p')
       AND NOT EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conrelid = c.oid AND k.contype = 'p')
     ORDER BY 1`);
  record(missing.length === 0, `${tag} every ferry table has a primary key`, missing.map((r) => r.relname).join(', '));
}

// Postgres does not index foreign keys itself; without one, a delete on the parent scans the child.
async function checkForeignKeyIndexes(client, tag) {
  const missing = await rows(client, `
    SELECT c.conrelid::regclass::text AS tbl, c.conname
      FROM pg_constraint c JOIN pg_namespace n ON n.oid = c.connamespace
     WHERE c.contype = 'f' AND n.nspname = 'ferry'
       AND NOT EXISTS (
         SELECT 1 FROM pg_index i
          WHERE i.indrelid = c.conrelid
            AND (string_to_array(i.indkey::text, ' ')::int2[])[1] = ANY (c.conkey))
     ORDER BY 1, 2`);
  record(
    missing.length === 0,
    `${tag} every ferry foreign key has an index led by one of its columns`,
    missing.map((r) => `${r.tbl}.${r.conname}`).join(', '),
  );
}

async function checkPublicUntouched(client, tag, before, extBefore) {
  const after = await publicObjects(client);
  const added = [...after].filter((o) => !before.has(o));
  record(added.length === 0, `${tag} nothing Ferry-made in public`, added.join(', '));
  const extAfter = await extensions(client);
  const newExt = [...extAfter].filter((e) => !extBefore.has(e));
  record(newExt.length === 0, `${tag} no extensions created`, newExt.join(', '));
}

async function checkViews(client, tag) {
  const views = await rows(client, `
    SELECT c.relname, coalesce('security_invoker=true' = ANY (c.reloptions), false) AS invoker
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'ferry' AND c.relkind IN ('v', 'm')
     ORDER BY 1`);
  const names = views.map((v) => v.relname);
  const missing = ['delivery_minutes_daily', 'impact_daily'].filter((v) => !names.includes(v));
  const notInvoker = views.filter((v) => !v.invoker).map((v) => v.relname);
  const failures = [];
  for (const view of names) {
    try {
      await client.query(`SELECT * FROM ferry.${quoteIdent(view)}`);
    } catch (err) {
      failures.push(`${view}: ${err.message}`);
    }
  }
  record(
    missing.length === 0 && notInvoker.length === 0 && failures.length === 0,
    `${tag} views select without error, all security_invoker`,
    missing.length || notInvoker.length || failures.length
      ? [missing.length && `missing ${missing.join(', ')}`, notInvoker.length && `not security_invoker: ${notInvoker.join(', ')}`, ...failures].filter(Boolean).join('; ')
      : names.join(', '),
  );
}

// A small delivered run and one seeded as demo history, rolled back: the views' numbers, kept apart by
// is_demo_history, and constraints that guard money, receipts and sign-in.
async function checkSampleData(client, tag) {
  const problems = [];
  await client.query('BEGIN');
  try {
    await client.query(`
      INSERT INTO ferry.areas (id, name, center_lat, center_lng) VALUES ('area_t', 'Test', 35.9132, -79.0558);
      INSERT INTO ferry.restaurants (id, name, area_id, lat, lng, address, hours_today, price_level)
        VALUES ('rst_t', 'Test Tacos', 'area_t', 35.913211, -79.055094, '1 Test St', '11:00–22:00', 1);
      INSERT INTO ferry.menu_categories (id, restaurant_id, name) VALUES ('cat_t', 'rst_t', 'Tacos');
      INSERT INTO ferry.menu_items (id, restaurant_id, category_id, name, price_cents)
        VALUES ('itm_t', 'rst_t', 'cat_t', 'Taco', 500);
      INSERT INTO ferry.item_options (id, menu_item_id, name) VALUES ('opt_t', 'itm_t', 'Size');
      INSERT INTO ferry.item_choices (id, option_id, name, price_delta_cents) VALUES ('cho_t', 'opt_t', 'Half size', -100);
      INSERT INTO ferry.users (id, phone_e164, display_name, avatar_seed, bubble_color) VALUES
        ('usr_runner', '+19195550001', 'Runner', 'runner', '#F97316'),
        ('usr_customer', '+19195550002', 'Customer', 'customer', '#2563EB');
      INSERT INTO ferry.circles (id, name, emoji) VALUES ('cir_t', 'Test circle', '🌊');
      INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, created_via, leaving_at, window_closes_at)
        VALUES ('run_t', 'rst_t', 'usr_runner', 'cir_t', 'app', now(), now()),
               ('run_u', 'rst_t', 'usr_runner', 'cir_t', 'message', now(), now());
      INSERT INTO ferry.runs (id, restaurant_id, runner_id, circle_id, status, created_via, leaving_at, window_closes_at, is_demo_history)
        VALUES ('run_h', 'rst_t', 'usr_runner', 'cir_t', 'delivered', 'app', '2026-10-03T21:50:00Z', '2026-10-03T21:50:00Z', true);
      INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents, ferry_fee_cents,
          tip_cents, total_cents, dropoff_label, dropoff_lat, dropoff_lng, handoff_code, car_distance_m,
          co2_saved_g, created_at, delivered_at) VALUES
        ('ord_a', 'run_t', 'usr_customer', 'settled', 1000, 75, 150, 200, 1425, 'Fetzer Gym', 35.909152,
          -79.046905, '1234', 1609.344, 400, '2026-10-10T22:00:00Z', '2026-10-10T22:20:00Z'),
        ('ord_b', 'run_t', 'usr_customer', 'delivered', 1000, 75, 150, 200, 1425, 'Fetzer Gym', 35.909152,
          -79.046905, '5678', 1609.344, 400, '2026-10-10T22:00:00Z', '2026-10-10T22:30:00Z'),
        ('ord_c', 'run_t', 'usr_customer', 'funded', 1000, 75, 150, 200, 1425, 'Fetzer Gym', 35.909152,
          -79.046905, '9012', NULL, NULL, '2026-10-10T22:00:00Z', NULL);
      INSERT INTO ferry.orders (id, run_id, customer_id, status, subtotal_cents, tax_cents, ferry_fee_cents,
          tip_cents, total_cents, dropoff_label, dropoff_lat, dropoff_lng, handoff_code, car_distance_m,
          co2_saved_g, created_at, delivered_at, is_demo_history) VALUES
        ('ord_h', 'run_h', 'usr_customer', 'settled', 1000, 75, 150, 200, 1425, 'Fetzer Gym', 35.909152,
          -79.046905, '3456', 1609.344, 400, '2026-10-03T22:00:00Z', '2026-10-03T22:15:00Z', true);
      INSERT INTO ferry.order_lines (order_id, line_no, menu_item_id, name, qty, unit_price_cents, line_total_cents)
        VALUES ('ord_a', 0, 'itm_t', 'Taco', 2, 500, 1000);
      INSERT INTO ferry.order_events (order_id, run_id, status) VALUES ('ord_a', 'run_t', 'funded');
      INSERT INTO ferry.location_pings (run_id, user_id, ts, lat, lng, accuracy_m)
        VALUES ('run_t', 'usr_runner', now(), 35.91, -79.05, 8);
      INSERT INTO ferry.ledger_entries (id, kind, debit_account, credit_account, amount_cents, run_id, order_id, idempotency_key)
        VALUES ('txn_a', 'order_hold', 'platform:ferry:escrow', 'user:usr_customer:checking', 1425, 'run_t', 'ord_a', 'order_hold:ord_a');
      UPDATE ferry.runs SET receipt_path = 'run_t/receipt.jpg', receipt_total_cents = 2000 WHERE id = 'run_t';
      INSERT INTO ferry.message_spaces (space_id, is_group, dm_user_id, line_phone_e164)
        VALUES ('spc_t', false, 'usr_customer', '+19195550100');
      INSERT INTO ferry.verify_challenges (id, phone_e164, code_hash, expires_at)
        VALUES ('chl_t', '+19195550003', encode(sha256(convert_to('048213', 'UTF8')), 'hex'), now() + interval '10 minutes');
      UPDATE ferry.verify_challenges SET attempts = attempts + 1 WHERE id = 'chl_t';
      UPDATE ferry.verify_challenges SET status = 'verified', user_id = 'usr_customer', is_new = false, verified_at = now()
        WHERE id = 'chl_t';`);

    const close = (a, b) => (typeof b === 'number' ? Math.abs(a - b) <= 1e-9 : a === b);
    const expectRows = (view, got, want) => {
      if (got.length !== want.length || want.some((w, i) => Object.entries(w).some(([k, v]) => !close(got[i][k], v)))) {
        problems.push(`${view} gave ${JSON.stringify(got)}, want ${JSON.stringify(want)}`);
      }
    };
    expectRows('impact_daily',
      await rows(client, 'SELECT day::text, circle_id, deliveries::int, car_miles_avoided, co2_saved_g, is_demo_history FROM ferry.impact_daily ORDER BY day'),
      [
        { day: '2026-10-03', circle_id: 'cir_t', deliveries: 1, car_miles_avoided: 1, co2_saved_g: 400, is_demo_history: true },
        { day: '2026-10-10', circle_id: 'cir_t', deliveries: 2, car_miles_avoided: 2, co2_saved_g: 800, is_demo_history: false },
      ]);
    expectRows('delivery_minutes_daily',
      await rows(client, 'SELECT day::text, restaurant_id, p50, p90, is_demo_history FROM ferry.delivery_minutes_daily ORDER BY day'),
      [
        { day: '2026-10-03', restaurant_id: 'rst_t', p50: 15, p90: 15, is_demo_history: true },
        { day: '2026-10-10', restaurant_id: 'rst_t', p50: 25, p90: 29, is_demo_history: false },
      ]);

    const mustFail = [
      ['an id without its prefix', "INSERT INTO ferry.circles (id, name, emoji) VALUES ('circle_x', 'x', 'x')"],
      ['a total that is not the sum of its parts', "UPDATE ferry.orders SET total_cents = total_cents + 1 WHERE id = 'ord_c'"],
      ['a status outside RunStatus', "UPDATE ferry.runs SET status = 'done' WHERE id = 'run_t'"],
      ['a receipt path outside the run folder', "UPDATE ferry.runs SET receipt_path = 'run_u/receipt.jpg' WHERE id = 'run_t'"],
      ['a delivered order without delivered_at', "UPDATE ferry.orders SET status = 'delivered' WHERE id = 'ord_c'"],
      ['a ledger amount of zero', "INSERT INTO ferry.ledger_entries (id, kind, debit_account, credit_account, amount_cents, idempotency_key) VALUES ('txn_z', 'deposit', 'user:usr_customer:checking', 'external', 0, 'z')"],
      ['a reused idempotency key', "INSERT INTO ferry.ledger_entries (id, kind, debit_account, credit_account, amount_cents, run_id, order_id, idempotency_key) VALUES ('txn_b', 'order_hold', 'platform:ferry:escrow', 'user:usr_customer:checking', 1425, 'run_t', 'ord_a', 'order_hold:ord_a')"],
      ['a ledger order on another run', "INSERT INTO ferry.ledger_entries (id, kind, debit_account, credit_account, amount_cents, run_id, order_id, idempotency_key) VALUES ('txn_c', 'refund', 'user:usr_customer:checking', 'platform:ferry:escrow', 10, 'run_u', 'ord_a', 'c')"],
      ['a malformed ledger account', "INSERT INTO ferry.ledger_entries (id, kind, debit_account, credit_account, amount_cents, idempotency_key) VALUES ('txn_d', 'deposit', 'usr_customer', 'external', 10, 'd')"],
      ['a challenge with a malformed phone', "INSERT INTO ferry.verify_challenges (id, phone_e164, code_hash, expires_at) VALUES ('chl_x', '9195550003', 'h', now())"],
      ['a negative attempt count', "UPDATE ferry.verify_challenges SET attempts = -1 WHERE id = 'chl_t'"],
      ['a malformed line phone on a space', "UPDATE ferry.message_spaces SET line_phone_e164 = '919-555-0100' WHERE space_id = 'spc_t'"],
      ['a duplicate phone number', "INSERT INTO ferry.users (id, phone_e164, avatar_seed, bubble_color) VALUES ('usr_dup', '+19195550001', 'd', '#000000')"],
      ['a deleted run that money points at', "DELETE FROM ferry.runs WHERE id = 'run_t'"],
    ];
    const accepted = [];
    for (const [label, sql] of mustFail) {
      await client.query('SAVEPOINT probe');
      try {
        await client.query(sql);
        accepted.push(label);
      } catch {
        // Rejected, as it should be.
      }
      await client.query('ROLLBACK TO SAVEPOINT probe');
    }
    if (accepted.length) problems.push(`accepted ${accepted.join('; ')}`);
    record(problems.length === 0, `${tag} views compute a sample delivered run apart from demo history; ${mustFail.length} bad writes rejected (all rolled back)`, problems.join('; '));
  } catch (err) {
    record(false, `${tag} views compute a sample delivered run apart from demo history (rolled back)`, err.message);
  } finally {
    await client.query('ROLLBACK').catch(() => {});
  }
}

async function checkApiRolePrivileges(client, tag, serverVersion, sqlxSchema) {
  const tablePrivs = ['SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER'];
  if (serverVersion >= 170000) tablePrivs.push('MAINTAIN');

  const schema = await rows(client, `
    SELECT r AS role, p AS priv FROM unnest($1::text[]) r, unnest(ARRAY['USAGE', 'CREATE']) p
     WHERE has_schema_privilege(r, 'ferry', p)`, [API_ROLES]);
  record(schema.length === 0, `${tag} anon and authenticated hold no privilege on schema ferry`, schema.map((g) => `${g.role} ${g.priv}`).join(', '));

  const relations = await rows(client, `
    SELECT r AS role, c.relname, c.relkind, p AS priv
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace, unnest($1::text[]) r, unnest($2::text[]) p
     WHERE n.nspname = 'ferry' AND c.relkind IN ('r', 'p', 'v', 'm', 'f') AND has_table_privilege(r, c.oid, p)
    UNION ALL
    SELECT r, c.relname, c.relkind, 'column ' || p
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace, unnest($1::text[]) r,
           unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'REFERENCES']) p
     WHERE n.nspname = 'ferry' AND c.relkind IN ('r', 'p', 'v', 'm', 'f') AND has_any_column_privilege(r, c.oid, p)`,
    [API_ROLES, tablePrivs]);
  const relCount = (await rows(client, `
    SELECT count(*)::int AS n FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'ferry' AND c.relkind IN ('r', 'p', 'v', 'm', 'f')`))[0].n;
  record(
    relations.length === 0,
    `${tag} anon and authenticated hold no privilege on any ferry table or view`,
    relations.length ? relations.slice(0, 10).map((g) => `${g.role} ${g.priv} on ${g.relname}`).join(', ') : `${relCount} relations, ${tablePrivs.length} privileges each`,
  );

  const sequences = await rows(client, `
    SELECT r AS role, c.relname, p AS priv
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace, unnest($1::text[]) r, unnest(ARRAY['USAGE', 'SELECT', 'UPDATE']) p
     WHERE n.nspname = 'ferry' AND c.relkind = 'S' AND has_sequence_privilege(r, c.oid, p)`, [API_ROLES]);
  const seqCount = (await rows(client, `
    SELECT count(*)::int AS n FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'ferry' AND c.relkind = 'S'`))[0].n;
  record(
    sequences.length === 0,
    `${tag} anon and authenticated hold no privilege on any ferry sequence`,
    sequences.length ? sequences.map((g) => `${g.role} ${g.priv} on ${g.relname}`).join(', ') : `${seqCount} sequences`,
  );

  const routines = await rows(client, `
    SELECT r AS role, p.oid::regprocedure::text AS routine
      FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace, unnest($1::text[]) r
     WHERE n.nspname = 'ferry' AND has_function_privilege(r, p.oid, 'EXECUTE')`, [API_ROLES]);
  const routineCount = (await rows(client, `
    SELECT count(*)::int AS n FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'ferry'`))[0].n;
  record(
    routines.length === 0,
    `${tag} anon and authenticated hold no privilege on any ferry function`,
    routines.length ? routines.map((g) => `${g.role} EXECUTE on ${g.routine}`).join(', ') : `${routineCount} functions`,
  );

  const defaults = await rows(client, `
    SELECT coalesce(n.nspname, '(all schemas)') AS schema, d.defaclobjtype AS objtype, a.grantee::regrole::text AS role
      FROM pg_default_acl d
      LEFT JOIN pg_namespace n ON n.oid = d.defaclnamespace
      CROSS JOIN LATERAL aclexplode(d.defaclacl) a
     WHERE (n.nspname = 'ferry' OR d.defaclnamespace = 0)
       AND a.grantee IN (SELECT oid FROM pg_roles WHERE rolname = ANY ($1))`, [API_ROLES]);
  record(
    defaults.length === 0,
    `${tag} no default privileges in ferry for anon or authenticated`,
    defaults.map((d) => `${d.role} on ${d.objtype} in ${d.schema}`).join(', '),
  );

  const sqlx = await rows(client, `
    SELECT c.relrowsecurity,
           ARRAY(SELECT r FROM unnest($1::text[]) r
                  WHERE has_table_privilege(r, c.oid, 'SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER')
                     OR has_any_column_privilege(r, c.oid, 'SELECT, INSERT, UPDATE, REFERENCES')) AS roles
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE c.relname = '_sqlx_migrations' AND c.relkind = 'r' AND n.nspname = $2`, [API_ROLES, sqlxSchema]);
  record(
    sqlx.length === 1 && sqlx[0].relrowsecurity && sqlx[0].roles.length === 0,
    `${tag} sqlx's ${sqlxSchema}._sqlx_migrations: RLS on, anon and authenticated revoked${sqlxSchema === 'public' ? ' despite Supabase-style public grants' : ''}`,
    sqlx.length !== 1 ? 'table missing' : !sqlx[0].relrowsecurity || sqlx[0].roles.length ? `RLS ${sqlx[0].relrowsecurity ? 'on' : 'off'}; privileges held by ${sqlx[0].roles.join(', ') || 'none'}` : '',
  );
}

// Supabase grants the API roles everything in public by default; mirror that, so the run proves
// ferry stays closed even next to it and sqlx's table is locked down.
const SUPABASE_PUBLIC_GRANTS = `
  GRANT USAGE ON SCHEMA public TO anon, authenticated;
  ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON TABLES TO anon, authenticated;
  ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON SEQUENCES TO anon, authenticated;
  ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON FUNCTIONS TO anon, authenticated;`;

// ---------- Main ----------

const state = { admin: null, config: null, databases: [], createdRoles: [], cleanedUp: false };

async function dropDatabase(name) {
  await state.admin.query(`DROP DATABASE IF EXISTS ${quoteIdent(name)} WITH (FORCE)`);
}

async function cleanup() {
  if (state.cleanedUp || !state.admin) return null;
  state.cleanedUp = true;
  const problems = [];
  for (const db of state.databases) {
    try {
      await dropDatabase(db);
    } catch (err) {
      problems.push(`drop database ${db}: ${err.message}`);
    }
  }
  for (const role of state.createdRoles) {
    try {
      await state.admin.query(`DROP ROLE IF EXISTS ${quoteIdent(role)}`);
    } catch (err) {
      problems.push(`drop role ${role}: ${err.message}`);
    }
  }
  await state.admin.query('SELECT pg_advisory_unlock($1::bigint)', [LOCK_KEY]).catch(() => {});
  return problems;
}

async function runPhase(tag, dbName, migrations, { rolesPresent, serverVersion, sample }) {
  await state.admin.query(`CREATE DATABASE ${quoteIdent(dbName)} TEMPLATE template0`);
  state.databases.push(dbName);
  const client = new pg.Client({ ...state.config, database: dbName });
  await client.connect();
  try {
    if (rolesPresent) await client.query(SUPABASE_PUBLIC_GRANTS);
    // sqlx creates its table before the first migration; it is not Ferry-made, so snapshot after it.
    await client.query(SQLX_TABLE);
    const before = await publicObjects(client);
    const extBefore = await extensions(client);
    try {
      await applyMigrations(client, migrations);
      record(true, `${tag} migrations apply in version order, each in its own transaction`, `${migrations.length} files`);
    } catch (err) {
      record(false, `${tag} migrations apply in version order, each in its own transaction`, err.message);
      return;
    }
    await checkRls(client, tag);
    if (rolesPresent) await checkApiRolePrivileges(client, tag, serverVersion, 'public');
    await checkPublicUntouched(client, tag, before, extBefore);
    await checkViews(client, tag);
    await checkPrimaryKeys(client, tag);
    await checkForeignKeyIndexes(client, tag);
    if (sample) await checkSampleData(client, tag);
  } finally {
    await client.end();
  }
}

// What a startup could change in ferry or public: schemas, relations (with owner, ACL, RLS and
// options), columns, constraints, indexes, views, routines, policies, default privileges, and sqlx's rows.
async function fingerprint(client) {
  const [{ fp }] = await rows(client, `
    WITH ns AS (SELECT oid, nspname FROM pg_namespace WHERE nspname IN ('ferry', 'public')),
         rel AS (SELECT c.*, ns.nspname FROM pg_class c JOIN ns ON ns.oid = c.relnamespace)
    SELECT json_build_object(
      'schemas', (SELECT json_agg(json_build_array(n.nspname, n.nspowner::regrole::text, n.nspacl::text) ORDER BY n.nspname)
                    FROM pg_namespace n JOIN ns USING (oid)),
      'relations', (SELECT json_agg(json_build_array(nspname, relname, relkind, relowner::regrole::text, relacl::text,
                                                     relrowsecurity, relforcerowsecurity, reloptions) ORDER BY nspname, relname)
                      FROM rel),
      'columns', (SELECT json_agg(json_build_array(rel.nspname, rel.relname, a.attname, format_type(a.atttypid, a.atttypmod),
                                                   a.attnotnull, pg_get_expr(d.adbin, d.adrelid), a.attacl::text)
                                  ORDER BY rel.nspname, rel.relname, a.attnum)
                    FROM rel JOIN pg_attribute a ON a.attrelid = rel.oid
                    LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
                   WHERE a.attnum > 0 AND NOT a.attisdropped),
      'constraints', (SELECT json_agg(json_build_array(k.conrelid::regclass::text, k.conname, pg_get_constraintdef(k.oid))
                                      ORDER BY k.conrelid::regclass::text, k.conname)
                        FROM pg_constraint k JOIN ns ON ns.oid = k.connamespace),
      'indexes', (SELECT json_agg(pg_get_indexdef(rel.oid) ORDER BY pg_get_indexdef(rel.oid)) FROM rel WHERE relkind = 'i'),
      'views', (SELECT json_agg(json_build_array(relname, pg_get_viewdef(rel.oid)) ORDER BY relname) FROM rel WHERE relkind = 'v'),
      'routines', (SELECT json_agg(p.oid::regprocedure::text ORDER BY p.oid::regprocedure::text)
                     FROM pg_proc p JOIN ns ON ns.oid = p.pronamespace),
      'policies', (SELECT json_agg(json_build_array(schemaname, tablename, policyname) ORDER BY schemaname, tablename, policyname)
                     FROM pg_policies),
      'default_acl', (SELECT json_agg(json_build_array(d.defaclrole::regrole::text, d.defaclnamespace::regnamespace::text,
                                                       d.defaclobjtype, d.defaclacl::text)
                                      ORDER BY d.defaclrole, d.defaclnamespace, d.defaclobjtype)
                        FROM pg_default_acl d),
      'sqlx', (SELECT json_agg(json_build_array(version, description, installed_on, success, encode(checksum, 'hex'), execution_time)
                               ORDER BY version)
                 FROM ferry._sqlx_migrations)
    )::text AS fp`);
  return fp;
}

// ferry-core's real layout on a Supabase-like database: the migrations run as the database's owner,
// which is neither superuser nor BYPASSRLS (Supabase's postgres), so only ownership lets sqlx write its
// table after the lockdown enables RLS on it. Then a second startup on a new connection.
async function runCoreBasePhase(tag, dbName, migrations, { serverVersion }) {
  await state.admin.query(`CREATE DATABASE ${quoteIdent(dbName)} OWNER ${quoteIdent(MIGRATOR_ROLE)} TEMPLATE template0`);
  state.databases.push(dbName);
  const connectAsOwner = async () => {
    const c = new pg.Client({ ...state.config, database: dbName });
    await c.connect();
    await c.query(`SET ROLE ${quoteIdent(MIGRATOR_ROLE)}`);
    return c;
  };
  const applyName = `${tag} migrations apply as ferry-core starts: search_path ferry, public; schema ferry first; sqlx's table in ferry; each file in its own transaction`;
  let client = await connectAsOwner();
  try {
    const [who] = await rows(client, `
      SELECT current_user AS role, r.rolsuper, r.rolbypassrls, d.datdba = r.oid AS owns_db
        FROM pg_roles r, pg_database d WHERE r.rolname = current_user AND d.datname = current_database()`);
    if (who.rolsuper || who.rolbypassrls || !who.owns_db) {
      record(false, applyName, `${who.role} must own the database and be neither superuser nor BYPASSRLS`);
      return;
    }
    await client.query(SUPABASE_PUBLIC_GRANTS);
    const before = await publicObjects(client);
    const extBefore = await extensions(client);
    try {
      const ran = await startLikeFerryCore(client, migrations);
      const [layout] = await rows(client, `
        SELECT to_regclass('ferry._sqlx_migrations') IS NOT NULL AS in_ferry,
               to_regclass('public._sqlx_migrations') IS NOT NULL AS in_public,
               (SELECT count(*)::int FROM ferry._sqlx_migrations WHERE success AND execution_time >= 0) AS done`);
      const ok = ran.length === migrations.length && layout.in_ferry && !layout.in_public && layout.done === migrations.length;
      record(ok, applyName, ok
        ? `${ran.length} files as ${who.role}, the database owner, not superuser, no BYPASSRLS; ferry._sqlx_migrations has ${layout.done} rows with execution_time set after COMMIT`
        : `applied ${ran.length} of ${migrations.length}; table in ferry: ${layout.in_ferry}; in public: ${layout.in_public}; finished rows: ${layout.done}`);
      if (!ok) return;
    } catch (err) {
      record(false, applyName, err.message);
      return;
    }
    await checkRls(client, tag);
    await checkApiRolePrivileges(client, tag, serverVersion, 'ferry');
    await checkPublicUntouched(client, tag, before, extBefore);
    await checkViews(client, tag);
    await checkPrimaryKeys(client, tag);
    await checkForeignKeyIndexes(client, tag);
    await checkSampleData(client, tag);

    const againName = `${tag} a second startup applies nothing and changes nothing`;
    const fpBefore = await fingerprint(client);
    await client.end();
    client = await connectAsOwner();
    try {
      const ran = await startLikeFerryCore(client, migrations);
      const same = (await fingerprint(client)) === fpBefore;
      record(ran.length === 0 && same, againName, ran.length
        ? `applied ${ran.join(', ')}`
        : same ? `${migrations.length} checksums match; catalog and _sqlx_migrations unchanged` : 'the catalog or _sqlx_migrations changed');
    } catch (err) {
      record(false, againName, err.message);
    }
  } finally {
    await client.end();
  }
}

async function main() {
  const { url, source } = resolveUrl();
  state.config = localConfig(url);
  const migrations = loadMigrations();

  state.admin = new pg.Client(state.config);
  await state.admin.connect();
  const serverVersion = Number((await rows(state.admin, 'SHOW server_version_num'))[0].server_version_num);
  const serverLabel = (await rows(state.admin, 'SHOW server_version'))[0].server_version;
  console.log(`check-migrations: ${migrations.length} migrations from backend/migrations on Postgres ${serverLabel} at ${state.config.host}:${state.config.port} (${source})`);

  if (!(await rows(state.admin, 'SELECT pg_try_advisory_lock($1::bigint) AS ok', [LOCK_KEY]))[0].ok) {
    note('waiting for another check-migrations run on this cluster to finish');
    await state.admin.query('SELECT pg_advisory_lock($1::bigint)', [LOCK_KEY]);
  }

  // Holding the lock, anything this script marked is a leftover from an interrupted run.
  for (const { datname } of await rows(state.admin, "SELECT datname FROM pg_database WHERE starts_with(datname, $1)", [DB_PREFIX])) {
    await dropDatabase(datname);
    note(`dropped ${datname}, left by an interrupted earlier run`);
  }
  const roleRows = await rows(state.admin, `
    SELECT rolname, shobj_description(oid, 'pg_authid') AS comment FROM pg_roles WHERE rolname = ANY ($1)`, [MARKED_ROLES]);
  const foreignRoles = [];
  for (const r of roleRows) {
    if (r.comment === ROLE_MARK) {
      await state.admin.query(`DROP ROLE ${quoteIdent(r.rolname)}`);
      note(`dropped role ${r.rolname}, left by an interrupted earlier run`);
    } else {
      foreignRoles.push(r.rolname);
    }
  }

  const suffix = `${process.pid}`;
  const foreignApiRoles = foreignRoles.filter((r) => API_ROLES.includes(r));
  if (foreignApiRoles.length) {
    record(false, '[roles absent] migrations on plain Postgres', `cannot test: role ${foreignApiRoles.join(' and ')} already exists in this cluster and was not created by this script`);
  } else {
    await runPhase('[roles absent]', `${DB_PREFIX}${suffix}_plain`, migrations, { rolesPresent: false, serverVersion, sample: true });
  }

  for (const role of API_ROLES.filter((r) => !foreignRoles.includes(r))) {
    await state.admin.query(`CREATE ROLE ${quoteIdent(role)} NOLOGIN`);
    state.createdRoles.push(role);
    await state.admin.query(`COMMENT ON ROLE ${quoteIdent(role)} IS '${ROLE_MARK}'`);
  }
  await runPhase('[roles present]', `${DB_PREFIX}${suffix}_supabase`, migrations, { rolesPresent: true, serverVersion, sample: false });

  // With anon and authenticated still present, as on Supabase.
  if (foreignRoles.includes(MIGRATOR_ROLE)) {
    record(false, '[core-base layout] migrations as ferry-core runs them', `cannot test: role ${MIGRATOR_ROLE} already exists in this cluster and was not created by this script`);
  } else {
    await state.admin.query(`CREATE ROLE ${quoteIdent(MIGRATOR_ROLE)} NOLOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE`);
    state.createdRoles.push(MIGRATOR_ROLE);
    await state.admin.query(`COMMENT ON ROLE ${quoteIdent(MIGRATOR_ROLE)} IS '${ROLE_MARK}'`);
    await runCoreBasePhase('[core-base layout]', `${DB_PREFIX}${suffix}_corebase`, migrations, { serverVersion });
  }

  const dbs = state.databases.join(', ');
  const roles = state.createdRoles.join(', ') || 'none';
  const problems = await cleanup();
  const leftRoles = (await rows(state.admin, 'SELECT rolname FROM pg_roles WHERE rolname = ANY ($1)', [MARKED_ROLES]))
    .map((r) => r.rolname).filter((r) => !foreignRoles.includes(r));
  const leftDbs = await rows(state.admin, 'SELECT datname FROM pg_database WHERE starts_with(datname, $1)', [DB_PREFIX]);
  record(
    problems.length === 0 && leftRoles.length === 0 && leftDbs.length === 0,
    'cleanup: cluster left as found',
    problems.length || leftRoles.length || leftDbs.length
      ? [...problems, leftRoles.length && `roles still present: ${leftRoles.join(', ')}`, leftDbs.length && `databases still present: ${leftDbs.map((d) => d.datname).join(', ')}`].filter(Boolean).join('; ')
      : `dropped databases ${dbs}; dropped roles ${roles}`,
  );
}

for (const signal of ['SIGINT', 'SIGTERM']) {
  process.once(signal, async () => {
    console.error(`\ncheck-migrations: ${signal}, cleaning up`);
    await cleanup().catch(() => {});
    await state.admin?.end().catch(() => {});
    process.exit(130);
  });
}

try {
  await main();
} catch (err) {
  record(false, 'check-migrations ran to completion', err.message);
  const problems = await cleanup().catch((e) => [e.message]);
  if (problems?.length) record(false, 'cleanup: cluster left as found', problems.join('; '));
} finally {
  await state.admin?.end().catch(() => {});
}

const failed = results.filter((ok) => !ok).length;
console.log(failed ? `${failed} of ${results.length} checks FAILED` : `all ${results.length} checks PASS`);
process.exitCode = failed ? 1 : 0;
