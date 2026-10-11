// Validates live REST responses and stream events against contract/openapi.yaml,
// with the contract validator's own ajv (npm --prefix ../contract/scripts ci).
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'

const root = new URL('../../../contract/', import.meta.url)
const req = createRequire(new URL('scripts/package.json', root))
const { Ajv2020 } = req('ajv/dist/2020.js')
const addFormats = req('ajv-formats').default ?? req('ajv-formats')
const YAML = req('yaml')

const eventSchema = {
  'run.updated': 'RunUpdatedEvent',
  'order.updated': 'OrderUpdatedEvent',
  'runner.location': 'RunnerLocationEvent',
  'notification.created': 'NotificationCreatedEvent',
  'invite.updated': 'InviteUpdatedEvent',
  'wallet.updated': 'WalletUpdatedEvent',
}

export async function validator() {
  const doc = YAML.parse(readFileSync(new URL('openapi.yaml', root), 'utf8'))
  const ajv = new Ajv2020({ allErrors: true, strict: false })
  addFormats(ajv)
  ajv.addSchema({ $id: 'openapi.yaml', components: doc.components }, 'openapi.yaml')
  const rewrite = (s) => JSON.parse(JSON.stringify(s).replaceAll('"#/components/', '"openapi.yaml#/components/'))
  const routes = Object.entries(doc.paths).map(([tpl, ops]) => ({
    re: new RegExp('^' + tpl.replace(/\{[^}]+\}/g, '[^/]+') + '$'),
    tpl,
    ops,
  }))
  const cache = new Map()
  const compiled = (key, schema) => {
    if (!cache.has(key)) cache.set(key, ajv.compile(rewrite(schema)))
    return cache.get(key)
  }
  const errs = (v) => (v.errors ?? []).slice(0, 4).map((e) => `${e.instancePath || '/'} ${e.message}`)

  return {
    response(method, path, status, body) {
      // Prefer the literal path over a template (e.g. /v1/runs/run_demo vs /v1/runs/{id}).
      const route = routes.find((r) => r.tpl === path) ?? routes.find((r) => r.re.test(path))
      if (!route) return [`no contract path for ${path}`]
      const op = route.ops[method.toLowerCase()]
      if (!op) return [`no ${method} on ${route.tpl}`]
      const res = op.responses?.[String(status)] ?? op.responses?.[`${String(status)[0]}XX`] ?? op.responses?.default
      if (!res) return [`status ${status} not in contract for ${method} ${route.tpl}`]
      const schema = res.content?.['application/json']?.schema
      if (!schema) return []
      const v = compiled(`${method} ${route.tpl} ${status}`, schema)
      return v(body) ? [] : errs(v)
    },
    event(evt) {
      const name = eventSchema[evt?.type]
      if (!name) return evt?.type === 'pong' ? [] : [`unknown event type ${evt?.type}`]
      const v = compiled(name, { $ref: `#/components/schemas/${name}` })
      return v(evt) ? [] : errs(v)
    },
  }
}
