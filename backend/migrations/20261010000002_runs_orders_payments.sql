-- Ferry schema, part 2: runs, orders, pickup requests and the money ledger (spec sections 4 and 5).
-- Rows that money history points at are never cascade-deleted: those keys are ON DELETE RESTRICT.

-- Run. order_count, runner_location (the latest ferry.location_pings row) and eta_minutes are derived.
-- receipt_total_cents and receipt_path come from POST /v1/runs/{id}/purchased. is_demo_history marks
-- the past weeks demo mode seeds (Impact.includes_demo_history), apart from real activity.
CREATE TABLE ferry.runs (
    id                  text PRIMARY KEY CHECK (starts_with(id, 'run_')),
    restaurant_id       text NOT NULL REFERENCES ferry.restaurants (id) ON DELETE RESTRICT,
    runner_id           text NOT NULL REFERENCES ferry.users (id) ON DELETE RESTRICT,
    circle_id           text NOT NULL REFERENCES ferry.circles (id) ON DELETE RESTRICT,
    status              text NOT NULL DEFAULT 'open'
                        CHECK (status IN ('open', 'closed', 'purchased', 'delivering', 'delivered', 'cancelled')),
    created_via         text NOT NULL CHECK (created_via IN ('message', 'app')),
    leaving_at          timestamptz NOT NULL,
    window_closes_at    timestamptz NOT NULL,
    receipt_total_cents bigint CHECK (receipt_total_cents >= 0),
    receipt_path        text,
    is_demo_history     boolean NOT NULL DEFAULT false,
    created_at          timestamptz NOT NULL DEFAULT now(),
    -- Receipt photos live under the run's own folder in the receipts bucket (review checklist).
    CHECK (receipt_path IS NULL OR (starts_with(receipt_path, id || '/') AND strpos(receipt_path, '..') = 0))
);
ALTER TABLE ferry.runs ENABLE ROW LEVEL SECURITY;
CREATE INDEX runs_circle_id_status_idx ON ferry.runs (circle_id, status);
CREATE INDEX runs_restaurant_id_idx ON ferry.runs (restaurant_id);
CREATE INDEX runs_runner_id_idx ON ferry.runs (runner_id);
-- The window closer's sweep: open runs by closing time.
CREATE INDEX runs_open_window_closes_at_idx ON ferry.runs (window_closes_at) WHERE status = 'open';

-- Order. restaurant_id and runner come from the run, lines from order_lines, timeline from
-- order_events, receipt_url from the run's receipt_path (signed per request), eta_minutes from tracking.
-- The drop-off Place is copied inline. delivered_at, car_distance_m, walk_distance_m and co2_saved_g
-- are the distance and timing kept on the order after its run's location pings are deleted.
-- is_demo_history is its run's flag, copied so order lists and the views need no join to tell.
CREATE TABLE ferry.orders (
    id               text PRIMARY KEY CHECK (starts_with(id, 'ord_')),
    run_id           text NOT NULL REFERENCES ferry.runs (id) ON DELETE RESTRICT,
    customer_id      text NOT NULL REFERENCES ferry.users (id) ON DELETE RESTRICT,
    status           text NOT NULL DEFAULT 'funded'
                     CHECK (status IN ('funded', 'purchased', 'picked_up', 'delivered', 'settled', 'cancelled', 'refunded')),
    subtotal_cents   bigint NOT NULL CHECK (subtotal_cents >= 0),
    tax_cents        bigint NOT NULL CHECK (tax_cents >= 0),
    ferry_fee_cents  bigint NOT NULL CHECK (ferry_fee_cents >= 0),
    tip_cents        bigint NOT NULL CHECK (tip_cents >= 0),
    total_cents      bigint NOT NULL CHECK (total_cents >= 0),
    dropoff_place_id text REFERENCES ferry.places (id) ON DELETE SET NULL,
    dropoff_label    text NOT NULL,
    dropoff_lat      double precision NOT NULL CHECK (dropoff_lat BETWEEN -90 AND 90),
    dropoff_lng      double precision NOT NULL CHECK (dropoff_lng BETWEEN -180 AND 180),
    dropoff_note     text,
    handoff_code     text NOT NULL CHECK (handoff_code ~ '^[0-9]{4}$'),
    car_distance_m   double precision CHECK (car_distance_m >= 0),
    walk_distance_m  double precision CHECK (walk_distance_m >= 0),
    co2_saved_g      double precision CHECK (co2_saved_g >= 0),
    delivered_at     timestamptz,
    is_demo_history  boolean NOT NULL DEFAULT false,
    created_at       timestamptz NOT NULL DEFAULT now(),
    -- Target of the composite keys in ledger_entries and order_events, which pin an order to its run.
    UNIQUE (id, run_id),
    CHECK (total_cents = subtotal_cents + tax_cents + ferry_fee_cents + tip_cents),
    CHECK (status NOT IN ('delivered', 'settled') OR delivered_at IS NOT NULL)
);
ALTER TABLE ferry.orders ENABLE ROW LEVEL SECURITY;
CREATE INDEX orders_run_id_idx ON ferry.orders (run_id);
CREATE INDEX orders_customer_id_created_at_idx ON ferry.orders (customer_id, created_at DESC);
CREATE INDEX orders_dropoff_place_id_idx ON ferry.orders (dropoff_place_id) WHERE dropoff_place_id IS NOT NULL;

-- OrderLine. name and unit_price_cents are snapshots at order time; unit_price_cents is the item's
-- price plus its choices' deltas.
CREATE TABLE ferry.order_lines (
    order_id         text NOT NULL REFERENCES ferry.orders (id) ON DELETE CASCADE,
    line_no          integer NOT NULL CHECK (line_no >= 0),
    menu_item_id     text NOT NULL REFERENCES ferry.menu_items (id) ON DELETE RESTRICT,
    name             text NOT NULL,
    qty              integer NOT NULL CHECK (qty > 0),
    choice_ids       text[] NOT NULL DEFAULT '{}',
    unit_price_cents bigint NOT NULL CHECK (unit_price_cents >= 0),
    line_total_cents bigint NOT NULL CHECK (line_total_cents >= 0),
    note             text,
    PRIMARY KEY (order_id, line_no),
    CHECK (line_total_cents = unit_price_cents * qty)
);
ALTER TABLE ferry.order_lines ENABLE ROW LEVEL SECURITY;
CREATE INDEX order_lines_menu_item_id_idx ON ferry.order_lines (menu_item_id);

-- PickupRequest (POST /v1/requests), an addition to section 5's list. lines is the request's
-- OrderLineInput[] as sent; matching creates an order on the run and records both ids here.
CREATE TABLE ferry.pickup_requests (
    id               text PRIMARY KEY CHECK (starts_with(id, 'req_')),
    customer_id      text NOT NULL REFERENCES ferry.users (id) ON DELETE CASCADE,
    restaurant_id    text NOT NULL REFERENCES ferry.restaurants (id) ON DELETE RESTRICT,
    lines            jsonb NOT NULL CHECK (jsonb_typeof(lines) = 'array'),
    tip_cents        bigint NOT NULL CHECK (tip_cents >= 0),
    dropoff_place_id text REFERENCES ferry.places (id) ON DELETE SET NULL,
    dropoff_label    text NOT NULL,
    dropoff_lat      double precision NOT NULL CHECK (dropoff_lat BETWEEN -90 AND 90),
    dropoff_lng      double precision NOT NULL CHECK (dropoff_lng BETWEEN -180 AND 180),
    dropoff_note     text,
    needed_by        timestamptz NOT NULL,
    status           text NOT NULL DEFAULT 'searching' CHECK (status IN ('searching', 'matched', 'expired')),
    run_id           text REFERENCES ferry.runs (id) ON DELETE RESTRICT,
    order_id         text REFERENCES ferry.orders (id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    CHECK (status <> 'matched' OR run_id IS NOT NULL)
);
ALTER TABLE ferry.pickup_requests ENABLE ROW LEVEL SECURITY;
CREATE INDEX pickup_requests_customer_id_created_at_idx ON ferry.pickup_requests (customer_id, created_at DESC);
CREATE INDEX pickup_requests_restaurant_id_idx ON ferry.pickup_requests (restaurant_id);
-- The matcher's sweep: requests still searching, by deadline.
CREATE INDEX pickup_requests_searching_needed_by_idx ON ferry.pickup_requests (needed_by) WHERE status = 'searching';
CREATE INDEX pickup_requests_run_id_idx ON ferry.pickup_requests (run_id) WHERE run_id IS NOT NULL;
CREATE INDEX pickup_requests_order_id_idx ON ferry.pickup_requests (order_id) WHERE order_id IS NOT NULL;
CREATE INDEX pickup_requests_dropoff_place_id_idx ON ferry.pickup_requests (dropoff_place_id) WHERE dropoff_place_id IS NOT NULL;

-- The double-entry ledger, the source of truth for money (spec section 4), with exactly section 4's
-- columns. Each row moves amount_cents out of credit_account into debit_account. Write the row with
-- its idempotency key first, call Nessie, then store nessie_object_id and nessie_status; a retry
-- finds the row by its key, so nothing posts twice. kind uses WalletTxnKind's strings and id the
-- txn_ prefix, so a row the viewer's account touches is a WalletTxn.
-- Accounts are 'external' (money from outside Ferry, such as a demo deposit) or
-- '<owner_kind>:<owner_id>:<purpose>', the key of the matching ferry.nessie_links row.
CREATE TABLE ferry.ledger_entries (
    id               text PRIMARY KEY CHECK (starts_with(id, 'txn_')),
    kind             text NOT NULL
                     CHECK (kind IN ('deposit', 'order_hold', 'refund', 'payout', 'card_funding', 'card_purchase')),
    debit_account    text NOT NULL CHECK (debit_account ~
                     '^(external|user:usr_[A-Za-z0-9_]+:(checking|ferry_card)|platform:ferry:escrow|restaurant:rst_[A-Za-z0-9_]+:(merchant|checking))$'),
    credit_account   text NOT NULL CHECK (credit_account ~
                     '^(external|user:usr_[A-Za-z0-9_]+:(checking|ferry_card)|platform:ferry:escrow|restaurant:rst_[A-Za-z0-9_]+:(merchant|checking))$'),
    amount_cents     bigint NOT NULL CHECK (amount_cents > 0),
    run_id           text REFERENCES ferry.runs (id) ON DELETE RESTRICT,
    order_id         text,
    idempotency_key  text NOT NULL UNIQUE,
    nessie_object_id text,
    nessie_status    text,
    created_at       timestamptz NOT NULL DEFAULT now(),
    CHECK (debit_account <> credit_account),
    -- An order's entries also name its run, and the composite key checks the pair.
    CHECK (order_id IS NULL OR run_id IS NOT NULL),
    FOREIGN KEY (order_id, run_id) REFERENCES ferry.orders (id, run_id) ON DELETE RESTRICT
);
ALTER TABLE ferry.ledger_entries ENABLE ROW LEVEL SECURITY;
CREATE INDEX ledger_entries_order_id_idx ON ferry.ledger_entries (order_id) WHERE order_id IS NOT NULL;
CREATE INDEX ledger_entries_run_id_idx ON ferry.ledger_entries (run_id) WHERE run_id IS NOT NULL;
-- Wallet: an account's entries, newest first.
CREATE INDEX ledger_entries_debit_account_idx ON ferry.ledger_entries (debit_account, created_at);
CREATE INDEX ledger_entries_credit_account_idx ON ferry.ledger_entries (credit_account, created_at);
-- The Nessie retry queue: rows written but not yet posted.
CREATE INDEX ledger_entries_unposted_idx ON ferry.ledger_entries (created_at) WHERE nessie_object_id IS NULL;

-- Ferry owners to Nessie objects (spec section 4). purpose says which Nessie object nessie_id is:
-- customer is a customer id; checking, escrow and ferry_card are account ids; merchant is a merchant
-- id. A runner's Ferry Card is an account under the platform customer, keyed by the runner.
CREATE TABLE ferry.nessie_links (
    owner_kind text NOT NULL CHECK (owner_kind IN ('user', 'platform', 'restaurant')),
    owner_id   text NOT NULL,
    purpose    text NOT NULL,
    nessie_id  text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_kind, owner_id, purpose),
    CHECK (
        (owner_kind = 'user' AND starts_with(owner_id, 'usr_') AND purpose IN ('customer', 'checking', 'ferry_card'))
        OR (owner_kind = 'platform' AND owner_id = 'ferry' AND purpose IN ('customer', 'escrow'))
        OR (owner_kind = 'restaurant' AND starts_with(owner_id, 'rst_') AND purpose IN ('merchant', 'customer', 'checking'))
    )
);
ALTER TABLE ferry.nessie_links ENABLE ROW LEVEL SECURITY;
