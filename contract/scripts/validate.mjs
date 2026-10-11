// Contract v1.5 validator. Run from anywhere:
//   npm --prefix contract/scripts ci && npm --prefix contract/scripts run validate
// Exits non-zero on any failure. Checks, in order:
//   1. contract/openapi.yaml lints as valid OpenAPI 3.1 with no errors (Redocly, recommended rules);
//   2. its operations are exactly the 43 method and path pairs of the contract: spec section 6's 39
//      plus the 5 that v1.2 adds, minus the 3 text sign-in operations and plus the 2 Telegram
//      sign-in operations of v1.3;
//   3. every contract/fixtures/*.json validates against its named schema ($refs resolved, formats
//      checked), and has no field the schema does not list;
//   4. each flattened schema (User, RunDetail) contains every property, with the same schema, and
//      every required entry of the schema it extends (PublicUser, Run); TelegramAuthStatus carries
//      AuthSession's properties with the same schemas (optional there, so not its required list).
import { execFileSync } from 'node:child_process';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { Ajv2020 } from 'ajv/dist/2020.js';
import addFormats from 'ajv-formats';
import { parse } from 'yaml';

const CONTRACT = join(import.meta.dirname, '..');
const SPEC = join(CONTRACT, 'openapi.yaml');
const FIXTURES = join(CONTRACT, 'fixtures');

// Spec section 6, Endpoints, with v1.2's and v1.3's changes (contract/CHANGELOG.md). /v1/stream is
// a WebSocket and is not in paths.
const OPERATIONS = [
  // v1.3: Telegram sign-in replaces section 6's and v1.2's /v1/auth/text/* (Sal's decision).
  'POST /v1/auth/telegram/start',
  'GET /v1/auth/telegram/{challenge_id}',
  'POST /v1/auth/demo',
  'GET /v1/me',
  'PUT /v1/me/profile',
  'GET /v1/me/stats',
  'GET /v1/areas',
  'GET /v1/restaurants',
  'GET /v1/restaurants/{id}/menu',
  'GET /v1/runs',
  'POST /v1/runs',
  'GET /v1/runs/{id}',
  'POST /v1/runs/{id}/orders',
  'POST /v1/runs/{id}/close',
  'POST /v1/runs/{id}/receipt-upload',
  'POST /v1/runs/{id}/purchased',
  'POST /v1/runs/{id}/picked-up',
  'POST /v1/runs/{id}/cancel',
  'GET /v1/orders',
  'GET /v1/orders/{id}',
  'POST /v1/orders/{id}/handoff',
  'POST /v1/orders/{id}/cancel',
  'POST /v1/requests',
  'POST /v1/runs/{id}/location',
  'GET /v1/runs/{id}/track',
  'GET /v1/circles',
  'GET /v1/circles/{id}',
  'POST /v1/circles/{id}/meet-again',
  'GET /v1/invites',
  'POST /v1/invites/{id}/respond',
  'GET /v1/notifications',
  'POST /v1/notifications/{id}/read',
  'GET /v1/public/push-key',
  'POST /v1/push/subscribe',
  'GET /v1/wallet',
  'POST /v1/wallet/demo-topup',
  'GET /v1/impact',
  'GET /v1/public/impact',
  'GET /v1/public/config',
  // v1.2: the frontend's contract requests (docs/CONTRACT_REQUESTS.md).
  'GET /v1/places',
  'POST /v1/runs/{id}/quote',
  'GET /v1/public/runs/{id}',
  'POST /v1/stream/ticket',
];

// Flattened schemas and the schema each one extends (docs/DECISIONS.md, Contract): the flattened
// one must carry every property and required entry of its base, so an additive change to the
// base cannot be missed in the copy. With required false, only the properties are compared: the
// poll's token and user are optional until the challenge is verified.
const FLATTENED = [
  ['User', 'PublicUser', { required: true }],
  ['RunDetail', 'Run', { required: true }],
  ['TelegramAuthStatus', 'AuthSession', { required: false }],
];

// Each fixture file and the schema it holds: an array of that schema.
const FIXTURE_SCHEMAS = {
  'users.json': 'User',
  'circles.json': 'Circle',
  'areas.json': 'Area',
  'restaurants.json': 'Restaurant',
  'menus.json': 'Menu',
  'places.json': 'Place',
  'runs.json': 'Run',
};

let failures = 0;
const fail = (msg) => {
  failures += 1;
  console.log(`  FAIL ${msg}`);
};

// 1. Lint.
console.log('1. lint contract/openapi.yaml (Redocly, OpenAPI 3.1, recommended rules)');
const redocly = join(import.meta.dirname, 'node_modules', '.bin', 'redocly');
try {
  const out = execFileSync(redocly, ['lint', SPEC, '--format=summary'], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, REDOCLY_TELEMETRY: 'off', REDOCLY_SUPPRESS_UPDATE_NOTICE: 'true' },
  });
  console.log(out.trim().split('\n').map((l) => `  ${l}`).join('\n'));
  console.log('  PASS lint: no errors');
} catch (err) {
  console.log(`${err.stdout ?? ''}${err.stderr ?? ''}`);
  fail('lint: redocly reported errors');
}

const doc = parse(readFileSync(SPEC, 'utf8'));

// 2. Operations.
console.log('2. operations match the contract (section 6 with v1.2 and v1.3)');
const METHODS = ['get', 'put', 'post', 'delete', 'patch', 'options', 'head', 'trace'];
const actual = Object.entries(doc.paths ?? {}).flatMap(([path, item]) =>
  METHODS.filter((m) => item[m]).map((m) => `${m.toUpperCase()} ${path}`),
);
const expected = new Set(OPERATIONS);
const missing = OPERATIONS.filter((op) => !actual.includes(op));
const extra = actual.filter((op) => !expected.has(op));
for (const op of missing) fail(`missing operation: ${op}`);
for (const op of extra) fail(`operation not in the OPERATIONS list: ${op}`);
const matched = actual.filter((op) => expected.has(op)).length;
if (missing.length === 0 && extra.length === 0) {
  console.log(`  PASS ${matched} of ${OPERATIONS.length} operations`);
} else {
  console.log(`  ${matched} of ${OPERATIONS.length} operations matched`);
}

// 3. Fixtures.
console.log('3. fixtures validate against their schemas');

// A copy of the components where every object schema with listed properties rejects unknown
// fields, so a misspelled fixture field fails here (the contract itself stays open).
const closeObjects = (node) => {
  if (Array.isArray(node)) return node.map(closeObjects);
  if (node === null || typeof node !== 'object') return node;
  const copy = Object.fromEntries(Object.entries(node).map(([k, v]) => [k, closeObjects(v)]));
  if (copy.properties && copy.additionalProperties === undefined) copy.additionalProperties = false;
  return copy;
};

const makeAjv = (components) => {
  const ajv = new Ajv2020({ allErrors: true, strict: false });
  addFormats(ajv);
  ajv.addSchema({ $id: 'openapi.yaml', components }, 'openapi.yaml');
  return ajv;
};
const ajv = makeAjv(doc.components);
const strictAjv = makeAjv(closeObjects(doc.components));

const files = readdirSync(FIXTURES).filter((f) => f.endsWith('.json')).sort();
for (const f of files) {
  if (!FIXTURE_SCHEMAS[f]) fail(`fixtures/${f}: no schema mapped in validate.mjs`);
}
for (const [file, name] of Object.entries(FIXTURE_SCHEMAS)) {
  if (!files.includes(file)) {
    fail(`fixtures/${file}: missing`);
    continue;
  }
  if (!doc.components?.schemas?.[name]) {
    fail(`fixtures/${file}: schema ${name} not in components/schemas`);
    continue;
  }
  let data;
  try {
    data = JSON.parse(readFileSync(join(FIXTURES, file), 'utf8'));
  } catch (err) {
    fail(`fixtures/${file}: not valid JSON (${err.message})`);
    continue;
  }
  const schema = { type: 'array', minItems: 1, items: { $ref: `openapi.yaml#/components/schemas/${name}` } };
  const checks = [
    ['schema', ajv.compile(schema)],
    ['no unknown fields', strictAjv.compile(schema)],
  ];
  let ok = true;
  for (const [label, validate] of checks) {
    if (!validate(data)) {
      ok = false;
      for (const e of validate.errors) {
        fail(`fixtures/${file} (${label}): ${e.instancePath || '/'} ${e.message} ${JSON.stringify(e.params)}`);
      }
    }
  }
  if (ok) console.log(`  PASS fixtures/${file}: ${data.length} x ${name}`);
}

// 4. Flattened schemas.
console.log('4. flattened schemas contain their base schemas');

// Structural equality of two parsed YAML values (key order ignored).
const sameSchema = (a, b) => {
  if (a === b) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  return ka.length === kb.length && ka.every((k) => Object.hasOwn(b, k) && sameSchema(a[k], b[k]));
};

for (const [flat, base, { required }] of FLATTENED) {
  const f = doc.components?.schemas?.[flat];
  const b = doc.components?.schemas?.[base];
  if (!f || !b) {
    fail(`flattened: ${!f ? flat : base} not in components/schemas`);
    continue;
  }
  let ok = true;
  for (const [prop, schema] of Object.entries(b.properties ?? {})) {
    if (!f.properties || !Object.hasOwn(f.properties, prop)) {
      ok = false;
      fail(`flattened: ${flat} lacks property ${prop} of ${base}`);
    } else if (!sameSchema(f.properties[prop], schema)) {
      ok = false;
      fail(`flattened: ${flat}.${prop} differs from ${base}.${prop}`);
    }
  }
  for (const req of required ? (b.required ?? []) : []) {
    if (!(f.required ?? []).includes(req)) {
      ok = false;
      fail(`flattened: ${flat}.required lacks ${req} (required in ${base})`);
    }
  }
  if (ok) {
    const n = Object.keys(b.properties ?? {}).length;
    const reqs = required ? `${(b.required ?? []).length} required` : 'required not compared';
    console.log(`  PASS ${flat} contains ${base}: ${n} properties, ${reqs}`);
  }
}

if (failures > 0) {
  console.log(`\nFAILED: ${failures} problem(s)`);
  process.exit(1);
}
console.log('\nOK: lint passed, operations match, every fixture is valid, flattened schemas contain their bases');
