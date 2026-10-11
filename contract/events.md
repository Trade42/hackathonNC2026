# Realtime events (Contract v1.5)

The WebSocket stream is P0. Its message types are schemas in `contract/openapi.yaml` (`StreamEnvelope`, `ServerEvent`, the six `*Event` and `*Data` schemas, and `ClientPing`), so clients generate types for them like any other.

## Connecting

```
{API_BASE}/v1/stream?ticket=<ticket>
{API_BASE}/v1/stream?token=<jwt>
```

- `ticket` (v1.2, preferred) comes from `POST /v1/stream/ticket` (bearer): `{ticket, expires_at}`. It is single use and valid about 60 s, so a URL that lands in a proxy or access log is useless afterwards. Get a fresh ticket for every connect and reconnect.
- `token` is the same JWT the REST API takes as `Authorization: Bearer <jwt>`, passed as a query parameter because browsers cannot set headers on a WebSocket. It still works.
- A missing, invalid, expired or already used ticket or token closes the connection. The client gets a new ticket (or signs in again) and reconnects.
- After a reconnect, the client refetches what it shows over REST (for example `GET /v1/runs/{id}` and `GET /v1/orders/{id}`), because events sent while it was disconnected are not replayed.

## The envelope

Every server message is one JSON object:

```json
{ "type": "order.updated", "data": { "order": { "id": "ord_8kq2m4", "...": "..." } }, "at": "2026-10-10T22:04:12Z" }
```

| Field | Type | Meaning |
|---|---|---|
| `type` | string | One of the six event types below. |
| `data` | object | The payload for that type. |
| `at` | string, ISO 8601 UTC | When the server emitted the event. |

## Server events

| `type` | `data` | When the backend sends it | To whom |
|---|---|---|---|
| `run.updated` | `{run: Run}` | A run is opened, changes status (open, closed, purchased, delivering, delivered, cancelled), or its order count changes, or one of its orders is handed off (v1.4.1). | Members of the run's circle. For a handoff that changes neither the run's status nor its order count, only that order's customer gets it, and their copy's `runner_location` and `eta_minutes` are null, since the order is no longer active (v1.4.1). Each copy's `run.runner_location` is null unless the recipient is the run's runner or has an active order on the run (status funded, purchased or picked_up), and null in every copy after delivery. |
| `order.updated` | `{order: Order}` | An order is placed or changes status (funded, purchased, picked_up, delivered, settled, cancelled, refunded), or its ETA, receipt or CO2 figure changes. | The order's customer and the run's runner. Each recipient gets the order as the REST API would show it to them: `handoff_code` is set only in the customer's copy, and `receipt_url` reaches only that run's runner and customers. |
| `runner.location` | `{run_id, location: LatLng, at, eta_minutes}` | The runner's app posts `POST /v1/runs/{id}/location` (about every 5 s) while the run is active. `at` inside `data` is when the location was measured; the age shown to users comes from it. | Only the run's runner and customers with an active order on that run (status funded, purchased or picked_up), and not after delivery. Nobody else gets it. |
| `notification.created` | `{notification: Notification}` | The backend creates a notification for a user (invite, run_open, order_update, payout, system). | The notification's user. |
| `invite.updated` | `{invite: Invite}` | A meet-again invite is created, a member responds, it expires, or its group thread changes state. | The invite's circle members. Each copy's `invite.thread.open_url` (v1.3) is set only for the inviter and the invitees who accepted, and null in every other copy. `invite.place` (v1.5) is the same in every copy. |
| `wallet.updated` | `{wallet: Wallet}` | The user's wallet changes: a deposit or demo top-up, an order hold, a refund, a payout, or a card funding or purchase. | The wallet's owner. |

The rule behind every row: a user gets events only for their own circles, runs and orders.

## Client messages

The client sends `{"type": "ping"}` every 25 s to keep the connection alive. The server sends nothing back for a ping; the client reconnects if the socket closes.

## Delivery rules

- Events are idempotent. The same event may arrive more than once, or after a REST response that already showed the change.
- Clients upsert by id: replace the stored run, order, notification, invite or wallet with the one in the event (`run.id`, `order.id`, `notification.id`, `invite.id`; the wallet belongs to the signed-in user). For `runner.location`, keep the location with the newest `at` per `run_id`.
- Event types are additive like the rest of the contract. A client ignores a `type` it does not know.
