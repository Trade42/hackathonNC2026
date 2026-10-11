# messaging

The Photon Spectrum sidecar (Bun and TypeScript). It forwards each inbound Telegram (or iMessage) event to ferry-core, sends the replies core returns, and gives core a small authenticated HTTP API for sending. It has no business logic (spec sections 2 and 3 in `docs/PROMPT-backend.md`). Telegram is the default; iMessage stays available behind `MESSAGING_PROVIDERS`. Photon facts and the decisions built on them are in `docs/DECISIONS.md`, Photon Spectrum.

## Run

Bun 1.4 is in `/home/sal/hackNCprograms/hk/bin` on the hackathon machine, so prefix commands with `PATH=/home/sal/hackNCprograms/hk/bin:$PATH`.

```sh
cd messaging
bun install
bun run start        # the sidecar
bun test             # unit and integration tests; no Photon, no Telegram, no network
bun x tsc --noEmit   # strict typecheck
```

`bun run start` and `bun test` pass `--no-env-file`, so Bun never auto-loads a `.env`; the sidecar's own loader does.

## Environment

The sidecar loads the one root `.env`: the file named by `FERRY_ENV_FILE` when that is set (process environment only), otherwise the first `.env` found walking up from the current directory in a directory that also holds `contract/`. The process environment always wins, even when a variable is set to empty, and empty means unset.

| Variable | Use | Default |
|---|---|---|
| `MESSAGING_PROVIDERS` | `telegram`, `imessage`, or `telegram,imessage`. Anything else stops startup. | `telegram` |
| `PHOTON_PROJECT_ID`, `PHOTON_PROJECT_SECRET` | Spectrum Cloud credentials, needed by **both** providers: Telegram's inbound reaches the sidecar through Photon's cloud (Fusor). If either is unset the sidecar logs one warning, never connects, and keeps serving the internal API (`spectrum: "disabled"`). | unset |
| `SPECTRUM_TELEGRAM_BOT_TOKEN` | The bot's token from @BotFather (`<bot id>:<secret>`). Unset or malformed: Telegram is disabled with one warning, never printing the value. | unset |
| `SPECTRUM_TELEGRAM_WEBHOOK_SECRET` | The webhook's `secret_token`: 1 to 256 characters of `A-Z`, `a-z`, `0-9`, `_` and `-` (make one with `openssl rand -hex 32`). Required with the token: when the token is set and this is unset or invalid, Telegram is disabled with one error line naming the variable, never its value, and the internal API still serves. Keep it the same across restarts (see Webhook secret below). Never logged. | unset |
| `TELEGRAM_BOT_USERNAME` | The bot's username; only printed at startup as the `t.me` link. | unset |
| `SIDECAR_SHARED_SECRET` | The `X-Ferry-Secret` value, both for calls to core and for every internal route. Required: the sidecar refuses to start without it. | none |
| `SIDECAR_URL` | Where core reaches the sidecar. The sidecar listens on its port (loopback when the host is loopback, otherwise all interfaces) and builds attachment URLs from it. | `http://127.0.0.1:8787` |
| `CORE_URL` | ferry-core's base URL. | `http://127.0.0.1:8080` |
| `SIDECAR_ECHO` | `true` answers every inbound event without calling core (the M0 proof, see Live echo test). | off |
| `AGENT_PHONE_E164` | iMessage only: the agent's own number, dropped from group participant lists when set. | unset |

With Photon's credentials, the token and the webhook secret set, Spectrum registers the bot's webhook on startup (cloud mode): Telegram posts updates to `https://<project slug>.spctrm.dev/telegram`, and Photon relays them to the sidecar over a websocket. The sidecar needs no public URL.

**Webhook secret.** The provider registers `SPECTRUM_TELEGRAM_WEBHOOK_SECRET` as the webhook's `secret_token`, and Telegram sends it in the `X-Telegram-Bot-Api-Secret-Token` header of every update. The provider refuses an update whose header is missing or different: Photon gets a 400, and the update never reaches the sidecar or core. So an update forged to the public `spctrm.dev` URL (any `from.id`, a fake `/start <code>`, a fake button press) is dropped. The SDK does not log these refusals; they show only in `getWebhookInfo` (Webhook checks, below).

The provider calls `setWebhook` only when the webhook's URL differs from the one Telegram has, and `getWebhookInfo` does not return the secret. So a restart never re-applies a secret: Telegram keeps sending the one it was first given, and if `.env` then holds a different one, every update is refused. Keep the value the same across restarts. To change it: stop the sidecar, delete the webhook once (Webhook checks), change the value in `.env`, and start; the provider registers the webhook again with the new secret. The same applies to a bot whose webhook was registered without a secret.

Logs mask every phone number and email (`+1•••0123`, `m•••@icloud.com`, also inside iMessage DM space ids), every Telegram user id and @username (last 3 characters kept: `•••321`, `@•••pez`; a Telegram DM's space id is the user's id, so it is masked too; a group's negative id is not), and any bot token. Names are never logged. Everything else printed through `console`, such as the Spectrum SDK's own warnings with their raw errors and stacks, is masked the same way (`src/console-mask.ts`, installed before spectrum-ts loads).

## Inbound: sidecar to core

For each inbound event the sidecar starts its own async task, so the Spectrum loop never waits. Tasks in the same space run in order, each with a 15 s timeout that cancels it: a task cut off by its timeout makes no further call to core and sends none of core's replies it has not already started, so a late reply never lands after the next message's. At most 20 events wait behind the running one in a space; past that the oldest waiting one is dropped, with a masked warning.

`POST {CORE_URL}/internal/messages/inbound` with `X-Ferry-Secret` and, for a Telegram group message:

```json
{
  "kind": "message",
  "channel": "telegram",
  "space_id": "-1001234567890",
  "is_group": true,
  "participants": ["987654321"],
  "sender_phone": null,
  "sender_id": "987654321",
  "sender_name": "Maya",
  "sender_username": "maya_l",
  "chat_title": "Hinton James 4th floor",
  "text": "heading to Cosmic Cantina at 6",
  "attachments": [{ "type": "image/jpeg", "url": "http://127.0.0.1:8787/attachments/<id>" }],
  "location": { "lat": 35.913211, "lng": -79.055094 },
  "received_at": "2026-10-10T22:00:00.000Z"
}
```

Every field of spec section 3 keeps its name. The fields core reads:

| Field | Telegram | iMessage |
|---|---|---|
| `kind` | `"message"`, `"callback"` (a button press) or `"membership"` (the bot added to, promoted in, or removed from a chat) | always `"message"` |
| `channel` | `"telegram"` | `"imessage"` |
| `space_id` | the chat id as a string: a DM's is the user's id (positive), a group's or supergroup's is negative. Stable; send into it later with `POST /send`. | the chat GUID: `any;-;<phone or email>` for a DM, a GUID containing `;+;` for a group |
| `is_group` | from the chat type: `private` is false, `group`, `supergroup` (and `channel`) are true | from the space type |
| `participants` | the sender's id only: the Bot API cannot list a group's members | the group's members plus the sender (see below) |
| `sender_phone` | `null`: Telegram has no phone numbers | the handle (E.164, or an email) |
| `sender_id` | the sender's Telegram user id as a string; this is how core identifies a Telegram user | the handle, same as `sender_phone` |
| `sender_name` | the first name, else `@username`; `null` if neither. A real name: never send it to Gemini. | `null` |
| `sender_username` | the username without `@`, or `null` (users need not have one). For `User.telegram_username`. | `null` |
| `chat_title` | the group's title; `null` in a DM | `null` |
| `text` | the text, or a photo's or file's caption; `""` for callbacks and membership | the text |
| `attachments` | photos (largest size), documents, voice, audio, video, stickers | photos, files, voice |
| `location` | a shared location pin or venue, or a map link in the text | a map link (an iPhone "Send My Current Location" card) |
| `received_at` | the message's time; for a callback, when the sidecar received it | the message's time |
| `callback_data` | only for `kind: "callback"`: the pressed button's `callback_data` | |
| `bot_in_chat` | only for `kind: "membership"`: whether the bot is in the chat now | |
| `bot_is_admin` | only for `kind: "membership"`: whether the bot is an administrator now (needed for invite links); `null` when Telegram did not say | |
| `migrated_from_space_id` | only for `kind: "membership"`, when Telegram upgraded a group to a supergroup: the old chat id. `space_id` is the new one; move whatever core keyed on the old id. | |

Telegram specifics:

- **Sign-in.** A user who opens `https://t.me/<bot>?start=<code>` and taps Start sends `/start <code>` as an ordinary DM message: `kind: "message"`, `text: "/start <code>"`, `sender_id` their user id. The sidecar forwards it unchanged; core turns it into sign-in.
- **Button presses.** The sidecar answers each press to Telegram at once (`answerCallbackQuery`, ahead of the space's queue, so the button's spinner stops whatever core does), then forwards it as `kind: "callback"`. `space_id` is the chat the button was in.
- **Membership.** The bot being added to or removed from a group, or promoted to admin, arrives as `kind: "membership"` with `bot_in_chat`, `bot_is_admin`, `chat_title`, and `sender_id` the person who did it. In a DM, `bot_in_chat: false` means the user blocked the bot (and `true` again that they unblocked it). Telegram's "X joined the group" service messages are not forwarded.
- **Not forwarded:** the bot's own messages and other bots', edits, reactions, channel posts, and service messages other than a group upgrade. Messages from a group admin who has "Remain anonymous" on come from Telegram's GroupAnonymousBot and are dropped too, so keep that setting off in the demo group.
- Bot privacy mode is off for @Ferrygo_bot, so the bot sees every group message.

iMessage specifics (unchanged):

- `participants`: a DM gives the sender; a group gives its members from Spectrum plus the sender, or just the sender if Spectrum cannot list them. The agent's own number is left out when it is known: `AGENT_PHONE_E164`, or a dedicated line's own number. **On the shared pool (the hackathon's Pro plan), a group's `participants` can include the pool number the agent uses there**: `imessage(space).phone` is the placeholder `"shared"` on the pool (spectrum-ts 12.10.1), so core must tolerate the agent's own number in `participants`.
- `sender_phone`, and each entry of `participants`, is a handle as iMessage reports it: usually E.164, but an email for an Apple ID without a phone number.
- `location` is present only when the message carries an Apple or Google Maps link with coordinates. Unverified on a real phone.

`attachments[].type` is the MIME type. `url` is served by this sidecar (`GET /attachments/<id>`, `X-Ferry-Secret` required) for 1 hour while the Spectrum connection lasts; a Telegram file is downloaded from the Bot API only when core fetches it.

Core answers `{replies: [{space_id, text}]}`. The sidecar sends each reply into its space, which may be a different chat from the inbound one. The timeout is 3 s with one retry (after a timeout, a network error or a 5xx). If both fail, no reply is sent and a masked warning is logged.

## Internal API: core to sidecar

Every route needs `X-Ferry-Secret: <SIDECAR_SHARED_SECRET>`, compared in constant time, or it returns 401. Errors use the contract's shape, `{"error": {"code", "message"}}`, with `details` where noted.

| Route | Body | Result |
|---|---|---|
| `GET /healthz` | | `200 {"status": "ok", "spectrum": "connected" \| "disconnected" \| "disabled", "providers": ["telegram"]}` (`providers`: the ones that connect) |
| `POST /send` | `{"text", "buttons"?}` plus exactly one of `"space_id"`, `"to_user_ids"` and `"to_phones"` | `200 {"space_id"}` |
| `POST /invite-links` | `{"space_id": "<group chat id>", "name"?}` (Telegram; name up to 32 characters) | `200 {"url": "https://t.me/+..."}`: a new invite link to a group where the bot is an admin with the right to invite users |
| `POST /groups` | `{"phones": [2 to 32 E.164], "name", "intro_text"}` | iMessage only: `200 {"space_id"}` after creating, naming and greeting the group. `501` on Telegram (bots cannot create groups) and on every shared-pool iMessage line. |
| `GET /attachments/<id>` | | the attachment's bytes with its MIME type, or `404` once expired |

`POST /send` targets:

- `space_id`: a Telegram chat id (a DM's is the user's id, a group's is negative) or an iMessage chat GUID; the sidecar tells them apart by shape.
- `to_user_ids`: Telegram, exactly one user id (string, or a JSON number). The DM's `space_id` is that id. Several ids are `501`: a bot cannot make a group chat, so send to each user.
- `to_phones`: iMessage. One phone opens or reuses the DM; several ask Spectrum for a group chat. On a sidecar without iMessage it is `400 phones_unsupported`.
- `buttons` (Telegram only): 1 to 8 inline buttons under the text, one per row. Each is `{"label", "url"}` (an `http`, `https` or `tg` link; the deep links `{web_url}/r/`, `/o/`, `/i/` go out this way) or `{"label", "callback_data"}` (1 to 64 bytes; a press comes back to core as `kind: "callback"`). Telegram rejects link buttons it considers invalid (likely including `localhost` URLs, unverified), which surfaces as `502 send_failed`, so `PUBLIC_WEB_URL` should be public.

Statuses:

| Status | Code | When |
|---|---|---|
| 400 | `invalid_request` | a bad body: text 1 to 10,000 characters (4,096 for Telegram), E.164 phones, Telegram user ids, buttons as above, no duplicates; `/invite-links` with a DM's id |
| 400 | `phones_unsupported` | `to_phones` without iMessage |
| 400 | `buttons_unsupported` | buttons to an iMessage chat |
| 400 | `channel_disabled` | a target on a provider this sidecar does not carry (not in `MESSAGING_PROVIDERS`, or without its credentials) |
| 403 | `recipient_not_reachable` | Telegram refused the send: the user never opened the bot and pressed Start, blocked it, or does not exist; or the bot is no longer in the group. Core should fall back (an app notification, the bot link). |
| 403 | `not_admin` | `/invite-links`: the bot is not an administrator of the group (with the right to invite users), or not in it |
| 409 | `chat_migrated` | the group became a supergroup; `details.space_id` is the new chat id |
| 501 | `unsupported` | `/groups` on Telegram, several `to_user_ids`, or a group on a shared iMessage line |
| 502 | `send_failed` | the platform rejected or timed out the send for another reason |
| 503 | `spectrum_unavailable` | Spectrum is disabled or reconnecting |

## Resilience

Every platform call made for core (`/send`, `/groups`, `/invite-links`, replies to inbound events, answers to button presses) has a 10 s timeout. The sidecar never retries a send that may have been delivered. On iMessage the SDK's gRPC client retries with idempotency keys; on Telegram the provider calls the Bot API over plain HTTP with no retry, so a failed Telegram send is reported (502, or the 4xx above) and core decides.

If the Spectrum stream ends or throws, or connecting takes longer than 30 s, the sidecar stops the app and connects again with backoff: 1 s, doubling to 30 s, back to 1 s after a connection that lasted 30 s. A connection that finishes after its 30 s is stopped at once, so two never run side by side. `/healthz` reports `disconnected` in between, and space handles from the old connection are dropped (sends then rebuild the space from its id).

## Live echo test (M0)

Only one sidecar may be connected to Photon with the real keys at a time, and only from the main checkout. Root `.env`: `PHOTON_PROJECT_ID`, `PHOTON_PROJECT_SECRET`, `SPECTRUM_TELEGRAM_BOT_TOKEN`, `SPECTRUM_TELEGRAM_WEBHOOK_SECRET` (made once with `openssl rand -hex 32`, then never changed), `TELEGRAM_BOT_USERNAME`, `SIDECAR_SHARED_SECRET`; leave `MESSAGING_PROVIDERS` unset (Telegram). Telegram must be enabled on the Photon project.

0. **Before the first start**, run the `getWebhookInfo` check (Webhook checks, below): `url` must be empty. If it is not, an earlier start registered the webhook without this secret: delete it once, then start.

```sh
cd /home/sal/hackNCprograms/repo/messaging
PATH=/home/sal/hackNCprograms/hk/bin:$PATH bun install
SIDECAR_ECHO=true PATH=/home/sal/hackNCprograms/hk/bin:$PATH bun run start
```

1. The log shows `providers=telegram`, `telegram bot link=https://t.me/Ferrygo_bot` and `spectrum connected`, and no `Telegram disabled` line. `curl -s -H "X-Ferry-Secret: <secret>" localhost:8787/healthz` shows `"spectrum":"connected","providers":["telegram"]`. The `getWebhookInfo` check now shows `url: "https://<project slug>.spctrm.dev/telegram"`.
2. **DM.** On a phone, open `https://t.me/Ferrygo_bot` and tap Start: the bot answers `echo: /start`, threaded to your message. Send `hi`: a typing indicator, then `echo: hi`. An echo means Telegram sends the secret: an update without it would have been refused.
3. **Group.** Create a Telegram group (the demo's is "Hinton James 4th floor") and add @Ferrygo_bot: the bot posts `echo: Ferry joined this group` (that is the membership event). Send `heading to Sup Dogs at 6`: `echo: heading to Sup Dogs at 6`. The sidecar's log line `echoed inbound message space=-100...` shows the group's chat id (group ids are not masked).
4. **Buttons.** With that group id:
   ```sh
   curl -s -H "X-Ferry-Secret: <secret>" -X POST localhost:8787/send -d '{"space_id":"<group id>","text":"Button test","buttons":[{"label":"Open Ferry","url":"https://github.com"},{"label":"Tap me","callback_data":"test:1"}]}'
   ```
   The message shows both buttons. Tap "Tap me": the spinner stops at once and the bot posts `echo: button test:1`.
5. **Invite link.** `curl ... -X POST localhost:8787/invite-links -d '{"space_id":"<group id>"}'` returns `403 not_admin`. Make the bot an administrator with "Invite users via link": it posts `echo: Ferry is an admin in this group` (if Telegram upgrades the group to a supergroup it posts `echo: this group moved to a new chat id`; use the new id from the log). The same call now returns `{"url":"https://t.me/+..."}`.
6. Remove the bot from the group: nothing is posted, and the log shows the membership event.
7. **A forged update is refused.** With the `url` from step 1: `curl -s -o /dev/null -w '%{http_code}\n' -X POST '<url>' -H 'content-type: application/json' -d '{"update_id":1,"message":{"message_id":1,"date":0,"chat":{"id":1,"type":"private"},"from":{"id":1,"is_bot":false,"first_name":"x"},"text":"forged"}}'`. Expect a 4xx (Photon passing on the provider's 400 is unverified), no echo, and no sidecar log line.

If the bot stops answering, run the `getWebhookInfo` check: a growing `pending_update_count` with a `last_error_message` means Telegram's updates are refused, most likely because `.env` holds a different secret from the one registered (see Webhook secret). If steps 3 to 5 show no membership or button events, look at `allowed_updates`. It should be `default`, which includes `callback_query` and `my_chat_member`. The provider never sends `allowed_updates`, and Telegram then keeps the bot's previous list, so a narrower list stays until a `setWebhook` call with the same URL, the same `secret_token` and `allowed_updates: []`.

### Webhook checks

These read the token from the root `.env` with the sidecar's own loader and never print it. `getWebhookInfo`:

```sh
cd /home/sal/hackNCprograms/repo/messaging
PATH=/home/sal/hackNCprograms/hk/bin:$PATH bun --no-env-file -e '
import { loadEnv } from "./src/env";
const method = process.env.TG_METHOD === "deleteWebhook" ? "deleteWebhook" : "getWebhookInfo";
const token = loadEnv().env.SPECTRUM_TELEGRAM_BOT_TOKEN;
try {
  const body = await (await fetch(`https://api.telegram.org/bot${token}/${method}`, { method: "POST" })).json();
  const r = body.result ?? {};
  if (!body.ok) console.log(`${method} refused: ${body.description}`);
  else if (method === "deleteWebhook") console.log("webhook deleted; the next start registers it again");
  else console.log({ url: r.url, pending_update_count: r.pending_update_count, last_error_date: r.last_error_date ? new Date(r.last_error_date * 1000).toISOString() : null, last_error_message: r.last_error_message ?? null, allowed_updates: r.allowed_updates ?? "default" });
} catch (e) {
  console.log(`${method} failed: ${e instanceof Error ? e.name : "unknown error"}`);
}'
```

It prints `url`, `pending_update_count`, `last_error_date`, `last_error_message` and `allowed_updates` (`default` when Telegram reports none). To delete the webhook (needed before a new secret takes effect), with the sidecar stopped, run the same command with `TG_METHOD=deleteWebhook` in front of it. Pending updates are kept, and the next start registers the webhook again.

For iMessage instead (shared pool: sending to recipients needs Photon's approval), set `MESSAGING_PROVIDERS=imessage`; the log shows `photon imessage line type=shared` (or `dedicated`), and texting the Photon number gets `echo: <your text>`.

## Layout

| File | What |
|---|---|
| `src/index.ts` | entry point: config, internal API, Spectrum supervisor |
| `src/env.ts` | the root `.env` loader |
| `src/config.ts` | settings from the environment; which providers connect, and why not |
| `src/spectrum.ts` | the only file that connects to Photon: one Spectrum app (Telegram, iMessage or both) as a `Transport` |
| `src/telegram-provider.ts` | spectrum-ts's Telegram provider with a wrapped inbound handler that also forwards callbacks, membership, chat titles and names |
| `src/telegram.ts` | raw Telegram Updates to events, chat ids, inline keyboards, Bot API error classification (no SDK imports) |
| `src/normalize.ts` | Spectrum content to text, attachments and location |
| `src/supervisor.ts` | connect, pump, reconnect with backoff |
| `src/bridge.ts` | inbound forwarding, echo, callback answers, sends, invite links, space and attachment handles |
| `src/core-client.ts` | the call to core: 3 s timeout, one retry |
| `src/server.ts` | the internal API (`Bun.serve`) |
| `src/timeout.ts` | `withTimeout`, shared by the bridge and the supervisor |
| `src/mask.ts`, `src/log.ts` | masking of phones, emails, Telegram ids, usernames and tokens; masked logs |
| `src/console-mask.ts`, `src/install-console-mask.ts` | masks all `console` output; `index.ts` imports the installer first |
| `test/` | `bun test` suites. `helpers.ts` has the fake Transport; `telegram-sdk.test.ts` drives the real SDK offline (Updates delivered as Photon's Fusor delivers them, a fake Bot API on a local port) |
