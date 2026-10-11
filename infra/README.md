# infra

## What is here

`db/check-migrations.mjs` is all this folder holds. There are no Dockerfiles or deploy config: Ferry has no cloud deploy (see Hosting, below).

The script applies every migration in `backend/migrations` to throwaway databases on a local Postgres, each in its own transaction as SQLx does, and checks schema `ferry`'s invariants: row level security on every table, nothing granted to `anon` or `authenticated`, nothing of Ferry's in `public`, and views that work. It runs the migrations three times: without Supabase's `anon` and `authenticated` roles, with them and Supabase's default grants, and in ferry-core's own layout, where a second startup must change nothing. It refuses any host but `127.0.0.1` or `localhost`, never prints the database URL, prints one PASS or FAIL line per check, and exits 1 on any FAIL.

```sh
cd infra/db && npm ci && node check-migrations.mjs    # reads TEST_DATABASE_URL, or takes a URL argument
```

Run it before any pull request that adds a migration.

## Hosting

The backend runs on the team's own always-on machine, not in the cloud. [docs/DECISIONS.md](../docs/DECISIONS.md) (Hosting) has the decision.

- ferry-core, the messaging sidecar and a Cloudflare tunnel each run there as a service that restarts on failure and starts on boot. The tunnel gives ferry-core its public HTTPS URL.
- Every minute a health check calls `/healthz` on that URL. It alerts the team by a Telegram message on the second failure in a row, and again on recovery.
- For now the tunnel is a Cloudflare quick tunnel, whose URL changes when it restarts; [docs/STATUS-backend.md](../docs/STATUS-backend.md) has the current one. Once ferrygo.tech's DNS is on Cloudflare, a named tunnel serves `https://api.ferrygo.tech`.
- The service definitions are not in this repo.
