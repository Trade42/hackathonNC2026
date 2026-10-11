# Contract changelog

Changes are additive only: new optional fields or new endpoints, one line each. Never a rename or a removal, except where Sal decides otherwise; the entry then says so.

## v1.5.1 (2026-10-11)

Additive only. Sal's decision (2026-10-11, 13:10 and 13:25 UTC): public communities, so anyone can order from anyone nearby, as on DoorDash.

- New optional `Circle.is_public` (boolean; the server always sends it). Behaviour: every user is in the public community UNC Chapel Hill (`cir_campus`), and `GET /v1/circles` lists public circles first. A public circle's `members` is always empty; `POST /v1/circles/{id}/meet-again` on one is 409 `conflict`; a run opened in one sends no `run_open` to its members (it shows in their `GET /v1/runs`), while its customers get every order notice as before.

## v1.5.0 (2026-10-11)

Additive only. Sal's decision (2026-10-11, about 01:08 EDT): meet-again becomes a ping with a place and a time, not a way back into an old group. The new fields are optional and nullable, as the coordination protocol's additive rule says; the server always sends `Invite.place`, as null when there is no place. Nothing is renamed or removed.

- New schema `InvitePlace {id: string | null, label: string, location: LatLng | null}`: a restaurant (`rst_` id), a known place (`place_` id), a map pin (label and location, no id), or words someone typed to the bot (a label alone). It is not `Place` because `Place.location` is required and a typed label has no coordinates; loosening `Place` would break every client that reads a `Place`.
- New optional, nullable `Invite.place` (`InvitePlace`), in every Invite response and in `invite.updated` (events.md). Every copy is the same.
- New optional `MeetAgainRequest.place` (`Place` or null). With an `id`, it must be a restaurant's (`rst_`) or a known place's (`place_`), and the invite takes that restaurant's name or that place's label and location; without one, a label of 1 to 100 characters and valid coordinates. Anything else is 400 `validation_failed`.
- Descriptions only: `POST /v1/circles/{id}/meet-again` says who gets the ping (the invite notification "Dana wants to go to Cosmic Cantina at 7:00 PM" and its push, and a Telegram DM with "I'm in" and "Can't make it" buttons for invitees who pressed Start), and `POST /v1/invites/{id}/respond` says each answer that changes a response is relayed to the inviter once (an invite notification, "Jordan's in." or "Maya can't make it.", and a DM when they pressed Start). `InviteThread.open_url` names the new button labels; the link is unchanged and stays an extra after "I'm in". The buttons' callback data is unchanged (`invite:<id>:accept|decline`).
- The fixtures are unchanged. The validator's package is 1.5.0 and still checks 43 operations.

## v1.4.1 (2026-10-11)

Docs only: `contract/events.md` now lists a `run.updated` trigger the backend has sent since 2026-10-11 (demo-walk-hook, #25). No schema, operation or fixture changes. `openapi.yaml` is unchanged and still says 1.4.0, and so is the validator.

- `run.updated` also goes out when one of a run's orders is handed off. When the handoff changes neither the run's status nor its order count, only that order's customer gets it, and their copy's `runner_location` and `eta_minutes` are null, since the order is no longer active. Their cached Run stops showing the runner while other orders are still out. The last handoff also makes the run delivered, a status change that every circle member already gets.

## v1.4.0 (2026-10-11)

Additive only: the frontend's delivery-app redesign requests "Going somewhere: return time, return place and drop-off radius", "Profile: how far I'll walk to drop off" and "Drop-off zone on orders and requests" (docs/CONTRACT_REQUESTS.md). Every new field is optional and nullable, as the coordination protocol's additive rule says; the server always sends the new response fields, as null when not given. A radius is whole meters from 50 to 5000. The fourth request, "Address search proxy (optional)", is deferred.

- New optional, nullable `Run.returning_at` (date-time), `Run.return_place` (`Place`) and `Run.dropoff_radius_m` (integer), the same three in `RunDetail` (so `run.updated` carries them too). `RunPreview` keeps its four fields.
- New optional `CreateRunRequest.returning_at`, `return_place` and `dropoff_radius_m`. `returning_at` must be after `leaving_at` and at most 24 hours after it; `return_place` a valid `Place` (an `id`, when given, from `GET /v1/places`); each problem is 400 `validation_failed`. When `dropoff_radius_m` is left out or null, the run takes the runner's `Profile.max_dropoff_distance_m`.
- New optional, nullable `Profile.max_dropoff_distance_m` (integer): `GET /v1/me` always sends it (null when unset), and `PUT /v1/me/profile` replaces it like every other answer, so leaving it out clears it.
- New optional `CreateOrderRequest.dropoff_radius_m` and `CreatePickupRequest.dropoff_radius_m`, echoed as optional, nullable `Order.dropoff_radius_m` and `PickupRequest.dropoff_radius_m`. The server stores it and does not compare it with the run's zone; the client warns. `POST /v1/runs/{id}/quote` takes the same body and checks the same range.
- `contract/events.md` says v1.4. The fixtures are unchanged. The validator's package is 1.4.0 and still checks 43 operations.

## v1.3.1 (2026-10-11)

Additive only. Telegram sign-in gets a matching confirm code: the frontend's contract request "Telegram sign-in: confirm in the bot before verifying (security)" (docs/CONTRACT_REQUESTS.md), on top of the Confirm button Sal added on 2026-10-10.

- New required `TelegramAuthChallenge.confirm_code: string`, four digits (`pattern: ^[0-9]{4}$`, leading zeros kept). The client shows it next to `bot_url` or the QR code: "Telegram will show 4821. Confirm only if it matches." It is required because it is only ever read: no v1.3 client breaks on a field it has not asked for yet.
- Descriptions only, `POST /v1/auth/telegram/start` and `GET /v1/auth/telegram/{challenge_id}`: tapping Start no longer verifies. The bot replies "Sign in to Ferry on another device? Check that the screen shows 4821." with **Confirm** and **Not me** buttons. The challenge turns `verified` only after Confirm from the same Telegram user, and Not me expires it.
- Description only, the poll: `token`, `user` and `is_new` come exactly once, on the first poll that sees `verified` (within a short time after Confirm). Every later poll answers `{"status": "verified"}` alone, so a client that missed them starts again.
- Description only, `User.telegram_username`: it no longer says the demo personas always have null, because `DEMO_TELEGRAM_PERSONAS` can link a persona to a Telegram account.
- The validator is unchanged: still 43 operations.

## v1.3 (2026-10-10)

Two changes break v1.2 clients, by Sal's decision to move messaging to Telegram after Photon's Pro plan refused iMessage: the removal of `/v1/auth/text/*` and the nullable `User.phone_masked`. Everything else in v1.3 is additive.

- **Breaking.** `POST /v1/auth/text/start`, `POST /v1/auth/text/{challenge_id}/verify` and `GET /v1/auth/text/{challenge_id}` are removed, with their schemas `StartTextAuthRequest`, `TextAuthChallenge`, `TextAuthState`, `TextAuthStatus`, `VerifyTextAuthRequest` and `TextAuthSession`. Telegram sign-in replaces them.
- New P0 `POST /v1/auth/telegram/start` (no body, no auth) → `TelegramAuthChallenge {challenge_id, bot_url, expires_at}`, where `bot_url` is `https://t.me/<bot>?start=<code>`. The client opens `bot_url` (on desktop it also shows it as a QR code), the user taps Start, and the bot receives `/start <code>` from that Telegram user. 429 `rate_limited` for too many starts; 503 `upstream_unavailable` (through `default`) when no bot is configured.
- New P0 `GET /v1/auth/telegram/{challenge_id}` (no auth) → `TelegramAuthStatus {status: "pending" | "verified" | "expired", token?, user?: User, is_new?}`, polled every 2 s; 404 `not_found` for an unknown id.
- **Breaking.** `User.phone_masked` is `string | null`: null for a user with no phone, which is every Telegram sign-up.
- New required, nullable `User.telegram_username: string | null`, without the leading @. The fixtures' users have `null`.
- New required, nullable `PublicConfig.agent.telegram_add_to_group_url: string | null` (`https://t.me/<bot>?startgroup=<code>`). `telegram_url` is now described as the bot's t.me link, and `imessage_phone_e164` as null (iMessage is off). Descriptions only for those two.
- New required, nullable `Invite.thread.open_url: string | null`: a Telegram link to the circle's group (the bot's invite link where it is an admin, else its startgroup link), only in the inviter's and accepted invitees' copies. `compose` stays and is null on Telegram.
- Descriptions only: the deep links `{web_url}/r/`, `/o/` and `/i/` are unchanged, and the bot sends them as link buttons (`info`, `PublicConfig.web_url`, `GET /v1/public/runs/{id}`); `invite.updated` carries `open_url` per recipient (events.md). The `ValidationFailed` and `RateLimited` example messages no longer mention codes or phones.
- The validator checks 43 operations (44, minus the 3 text operations, plus the 2 Telegram ones). Its fourth check keeps `User` ⊇ `PublicUser` and `RunDetail` ⊇ `Run`; `TextAuthSession` ⊇ `AuthSession` is gone with the schema, and `TelegramAuthStatus` must carry `AuthSession`'s `token` and `user` with identical schemas (optional there). Its package is now 1.3.0.

## v1.2 (2026-10-10)

The sign-in change and the nullable agent number are breaking, by Sal's decision for Photon's shared number pool (no single agent number). Everything else in v1.2 is additive.

- **Breaking.** `POST /v1/auth/text/start` is agent-texts-first: it takes a required body `{phone}` (E.164, `StartTextAuthRequest`) and returns `{challenge_id, expires_at}`. The agent texts a 6-digit code to that phone. `agent_phone_e164` and `body` are gone from `TextAuthChallenge`, and the "FERRY ####" text from the user no longer signs anyone in. New errors: 429 `rate_limited` (more than 3 codes per phone in 10 minutes) and 400 `validation_failed` (a phone that is not E.164).
- New P0 `POST /v1/auth/text/{challenge_id}/verify` with `{code}` (6 digits, as a string; `VerifyTextAuthRequest`) → `TextAuthSession {token, user, is_new}`. A wrong or expired code is 400 `validation_failed`; after 5 wrong codes the challenge expires; an unknown challenge is 404 `not_found`. No auth. `GET /v1/auth/text/{challenge_id}` is unchanged.
- **Breaking.** `PublicConfig.agent.imessage_phone_e164` is `string | null`: null on Photon's shared pool, where each user may see a different number. `display` stays a string: the agent's display name, "Ferry", when there is no number.
- Runner-location privacy, descriptions only (no wire change): `Run.runner_location`, `RunDetail.runner_location`, `Track.runner_location` and `Track.route`, and the `run.updated` and `runner.location` events, are null or not sent unless the viewer is the run's runner or has an active order on the run (funded, purchased or picked_up), and null after delivery.
- New P0 `GET /v1/places` → `Place[]`: the drop-off places.
- New P0 query parameter `mine` (boolean) on `GET /v1/runs`: with `mine=true`, the viewer's own runs as runner with status open, closed, purchased or delivering, in any circle.
- New P1 `POST /v1/runs/{id}/quote` with the `CreateOrderRequest` body → `OrderQuote {subtotal_cents, tax_cents, ferry_fee_cents, tip_cents, total_cents}`, with no hold and no side effects; 409 `window_closed` and 400 `validation_failed` as for placing an order.
- New P1 `GET /v1/public/runs/{id}` (no auth) → `RunPreview {restaurant_name, runner_display_name, window_closes_at, status}`, for iMessage link previews; 404 for an unknown id.
- New P1 `POST /v1/stream/ticket` → `StreamTicket {ticket, expires_at}`, single use and valid about 60 s; the WebSocket accepts `/v1/stream?ticket=` as well as `?token=` (events.md).
- Documentation: `POST /v1/runs/{id}/orders` lists its 400 `validation_failed` explicitly, and the new named error responses `ValidationFailed`, `NotFound` and `RateLimited` are in `components/responses`. The validator checks 44 operations and that `User`, `RunDetail` and `TextAuthSession` contain `PublicUser`, `Run` and `AuthSession`.

## v1.1 (2026-10-10)

- Initial contract: Contract v1.1 from spec section 6, written out as `openapi.yaml` (39 endpoints, every type, errors, security and the WebSocket message schemas), `events.md` and `fixtures/*.json`.
