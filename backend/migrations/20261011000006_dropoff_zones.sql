-- Ferry schema, part 11: the going-somewhere plan, drop-off radii and the profile's walk distance (contract
-- v1.4.0; docs/CONTRACT_REQUESTS.md, "Going somewhere: return time, return place and drop-off radius",
-- "Profile: how far I'll walk to drop off" and "Drop-off zone on orders and requests").
--
-- Every column is new and nullable with no default, so adding it rewrites nothing and every existing row
-- reads as "not given". Each CHECK allows null, so existing rows pass it.
--
-- runs: returning_at is when the runner expects to be back, after leaving_at (ferry-core also caps it at
-- 24 hours after). The return Place is copied inline like an order's drop-off: label, lat and lng together
-- or not at all, and a place id or note only with a label. dropoff_radius_m is how far from that place the
-- runner will walk to hand food over; POST /v1/runs fills it from the profile when the request leaves it out,
-- so it may be set without a return place.
--
-- profiles: max_dropoff_distance_m is the user's usual walk distance, the default for a new run's radius.
-- orders and pickup_requests: dropoff_radius_m is how far from the drop-off the customer will walk to meet.
-- Every radius is whole meters from 50 to 5000.

ALTER TABLE ferry.runs
    ADD COLUMN returning_at      timestamptz,
    ADD COLUMN return_place_id   text REFERENCES ferry.places (id) ON DELETE SET NULL,
    ADD COLUMN return_label      text,
    ADD COLUMN return_lat        double precision,
    ADD COLUMN return_lng        double precision,
    ADD COLUMN return_note       text,
    ADD COLUMN dropoff_radius_m  integer,
    ADD CONSTRAINT runs_returning_at_check
        CHECK (returning_at IS NULL OR returning_at > leaving_at),
    ADD CONSTRAINT runs_return_lat_check CHECK (return_lat BETWEEN -90 AND 90),
    ADD CONSTRAINT runs_return_lng_check CHECK (return_lng BETWEEN -180 AND 180),
    ADD CONSTRAINT runs_return_place_check
        CHECK ((return_label IS NULL) = (return_lat IS NULL) AND (return_lat IS NULL) = (return_lng IS NULL)),
    ADD CONSTRAINT runs_return_place_parts_check
        CHECK (return_label IS NOT NULL OR (return_place_id IS NULL AND return_note IS NULL)),
    ADD CONSTRAINT runs_dropoff_radius_m_check CHECK (dropoff_radius_m BETWEEN 50 AND 5000);
-- The foreign key's ON DELETE SET NULL looks runs up by return_place_id.
CREATE INDEX runs_return_place_id_idx ON ferry.runs (return_place_id) WHERE return_place_id IS NOT NULL;

ALTER TABLE ferry.profiles
    ADD COLUMN max_dropoff_distance_m integer,
    ADD CONSTRAINT profiles_max_dropoff_distance_m_check CHECK (max_dropoff_distance_m BETWEEN 50 AND 5000);

ALTER TABLE ferry.orders
    ADD COLUMN dropoff_radius_m integer,
    ADD CONSTRAINT orders_dropoff_radius_m_check CHECK (dropoff_radius_m BETWEEN 50 AND 5000);

ALTER TABLE ferry.pickup_requests
    ADD COLUMN dropoff_radius_m integer,
    ADD CONSTRAINT pickup_requests_dropoff_radius_m_check CHECK (dropoff_radius_m BETWEEN 50 AND 5000);
