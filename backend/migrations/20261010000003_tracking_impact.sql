-- Ferry schema, part 3: time series and impact views (spec section 5). Plain append-only tables in
-- place of TimescaleDB: rows are inserted, never updated. A run's location pings are deleted 24 hours
-- after delivery; distance and timing stay on the order.

-- LocationPing (POST /v1/runs/{id}/location). The run's latest ping is Run.runner_location.
CREATE TABLE ferry.location_pings (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    run_id     text NOT NULL REFERENCES ferry.runs (id) ON DELETE CASCADE,
    user_id    text NOT NULL REFERENCES ferry.users (id) ON DELETE CASCADE,
    ts         timestamptz NOT NULL,
    lat        double precision NOT NULL CHECK (lat BETWEEN -90 AND 90),
    lng        double precision NOT NULL CHECK (lng BETWEEN -180 AND 180),
    accuracy_m double precision NOT NULL CHECK (accuracy_m >= 0)
);
ALTER TABLE ferry.location_pings ENABLE ROW LEVEL SECURITY;
CREATE INDEX location_pings_run_id_ts_idx ON ferry.location_pings (run_id, ts);
CREATE INDEX location_pings_user_id_idx ON ferry.location_pings (user_id);

-- Order status changes: an order's rows, oldest first, are Order.timeline.
CREATE TABLE ferry.order_events (
    id       bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    order_id text NOT NULL,
    run_id   text NOT NULL REFERENCES ferry.runs (id) ON DELETE CASCADE,
    ts       timestamptz NOT NULL DEFAULT now(),
    status   text NOT NULL
             CHECK (status IN ('funded', 'purchased', 'picked_up', 'delivered', 'settled', 'cancelled', 'refunded')),
    payload  jsonb NOT NULL DEFAULT '{}',
    FOREIGN KEY (order_id, run_id) REFERENCES ferry.orders (id, run_id) ON DELETE CASCADE
);
ALTER TABLE ferry.order_events ENABLE ROW LEVEL SECURITY;
CREATE INDEX order_events_run_id_ts_idx ON ferry.order_events (run_id, ts);
CREATE INDEX order_events_order_id_ts_idx ON ferry.order_events (order_id, ts);

-- Delivered orders per UTC day and circle, and the car trips they replaced. Serves Impact (scopes
-- circle and campus, by_week) and Circle.stats.co2_saved_g. car_miles_avoided is from car_distance_m.
-- Rows are also split by is_demo_history, so seeded past weeks stay apart from real deliveries and
-- Impact.includes_demo_history is whether any summed row has it.
CREATE VIEW ferry.impact_daily WITH (security_invoker = true) AS
SELECT
    (o.delivered_at AT TIME ZONE 'UTC')::date          AS day,
    r.circle_id                                         AS circle_id,
    count(*)                                            AS deliveries,
    coalesce(sum(o.car_distance_m), 0) / 1609.344       AS car_miles_avoided,
    coalesce(sum(o.co2_saved_g), 0)                     AS co2_saved_g,
    o.is_demo_history                                   AS is_demo_history
FROM ferry.orders o
JOIN ferry.runs r ON r.id = o.run_id
WHERE o.status IN ('delivered', 'settled')
GROUP BY 1, 2, 6;

-- Minutes from placing an order to its delivery, per UTC day and restaurant: the median (p50) and
-- the 90th percentile (p90). The agent answers "how long does Sup Dogs usually take?" from it.
-- Split by is_demo_history like impact_daily; percentiles cannot be merged across the split.
CREATE VIEW ferry.delivery_minutes_daily WITH (security_invoker = true) AS
SELECT
    (o.delivered_at AT TIME ZONE 'UTC')::date AS day,
    r.restaurant_id                           AS restaurant_id,
    percentile_cont(0.5) WITHIN GROUP (
        ORDER BY extract(epoch FROM o.delivered_at - o.created_at)::double precision / 60
    )                                         AS p50,
    percentile_cont(0.9) WITHIN GROUP (
        ORDER BY extract(epoch FROM o.delivered_at - o.created_at)::double precision / 60
    )                                         AS p90,
    o.is_demo_history                         AS is_demo_history
FROM ferry.orders o
JOIN ferry.runs r ON r.id = o.run_id
WHERE o.status IN ('delivered', 'settled')
GROUP BY 1, 2, 5;
