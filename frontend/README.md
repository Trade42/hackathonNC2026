# React + TypeScript + Vite

This template provides a minimal setup to get React working in Vite with HMR and some Oxlint rules.

Currently, two official plugins are available:

- [@vitejs/plugin-react](https://github.com/vitejs/vite-plugin-react/blob/main/packages/plugin-react) uses [Oxc](https://oxc.rs)
- [@vitejs/plugin-react-swc](https://github.com/vitejs/vite-plugin-react/blob/main/packages/plugin-react-swc) uses [SWC](https://swc.rs/)

## React Compiler

The React Compiler is not enabled on this template because of its impact on dev & build performances. To add it, see [this documentation](https://react.dev/learn/react-compiler/installation).

## Expanding the Oxlint configuration

If you are developing a production application, we recommend enabling type-aware lint rules by installing `oxlint-tsgolint` and editing `.oxlintrc.json`:

```json
{
  "$schema": "./node_modules/oxlint/configuration_schema.json",
  "plugins": ["react", "typescript", "oxc"],
  "options": {
    "typeAware": true
  },
  "rules": {
    "react/rules-of-hooks": "error",
    "react/only-export-components": ["warn", { "allowConstantExport": true }]
  }
}
```

See the [Oxlint rules documentation](https://oxc.rs/docs/guide/usage/linter/rules) for the full list of rules and categories.

## Run modes

The tracked `.env` is **live**: `VITE_API_MODE=live`, `VITE_API_BASE_URL` (the live API) and `VITE_WS_URL` (the stream's base; the client appends `/v1/stream`). `.env.mock` switches to the MSW demo, which simulates the whole backend and pins its own base URLs, so it never reaches the live API.

| | live | mock |
|---|---|---|
| dev server | `npm run dev` | `npm run dev:mock` |
| build | `npm run build` | `npm run build:mock` |
| preview a build | `npm run preview:live` (port 5173, which the API's CORS allows) | `npm run preview` |
| tests | `npm run test:live` (the live pass) | `npm run test:e2e` (the Playwright suite) |

Process environment beats `.env` in Vite: a `VITE_API_MODE`, `VITE_API_BASE_URL` or `VITE_WS_URL` set in the shell or in Vercel's project settings overrides these files.

**The live pass** (`e2e/live/`) drives the built app against the live API at a phone size: demo sign-in as Jordan, Maya and Dana; the judge orders on `run_demo`; Maya closes, uploads a receipt, marks it purchased and picks up; the walk, Got it and the CO2 line; request a Ferry; meet-again with a place and its answer; the wallet, impact, notifications and the stream. It validates every REST response and stream event against `contract/openapi.yaml` and writes `e2e/live/out/results.json`. It places a real order on the demo data, so ask for a demo reset before and after each run.

```
npm --prefix ../contract/scripts ci   # once: the contract validator's ajv
npm run build && npm run preview:live # shell 1
npm run test:live                     # shell 2 (BASE=... to check another build)
```
