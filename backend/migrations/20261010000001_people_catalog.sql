-- Ferry schema, part 1: people, circles and the catalog.
--
-- Every Ferry object lives in schema ferry, never in public, and every query names it (ferry.users).
-- Each table enables row level security with no policies: the server connects as postgres, the owner,
-- so RLS does not block it, while anon and authenticated get nothing (20261010000005_lockdown.sql).
-- Field names follow contract/openapi.yaml; docs/DECISIONS.md (Schema) maps each table to its type.

CREATE SCHEMA IF NOT EXISTS ferry;

-- PublicUser and User. phone_masked is derived from phone_e164; the profile is ferry.profiles.
CREATE TABLE ferry.users (
    id                  text PRIMARY KEY CHECK (starts_with(id, 'usr_')),
    phone_e164          text NOT NULL UNIQUE CHECK (phone_e164 ~ '^\+[1-9][0-9]{1,14}$'),
    display_name        text NOT NULL DEFAULT '',
    avatar_seed         text NOT NULL,
    bubble_color        text NOT NULL CHECK (bubble_color ~ '^#[0-9A-Fa-f]{6}$'),
    onboarding_complete boolean NOT NULL DEFAULT false,
    created_at          timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE ferry.users ENABLE ROW LEVEL SECURITY;

-- Place: the named drop-off spots (fixtures: places.json). Orders and profiles copy a Place inline,
-- because Place.id is optional and a drop-off may be a spot that is not in this table.
CREATE TABLE ferry.places (
    id    text PRIMARY KEY CHECK (starts_with(id, 'place_')),
    label text NOT NULL,
    lat   double precision NOT NULL CHECK (lat BETWEEN -90 AND 90),
    lng   double precision NOT NULL CHECK (lng BETWEEN -180 AND 180),
    note  text
);
ALTER TABLE ferry.places ENABLE ROW LEVEL SECURITY;

-- Profile: the questionnaire answers (PUT /v1/me/profile). dropoff_default is a whole Place or null.
CREATE TABLE ferry.profiles (
    user_id                     text PRIMARY KEY REFERENCES ferry.users (id) ON DELETE CASCADE,
    dropoff_place_id            text REFERENCES ferry.places (id) ON DELETE SET NULL,
    dropoff_label               text,
    dropoff_lat                 double precision CHECK (dropoff_lat BETWEEN -90 AND 90),
    dropoff_lng                 double precision CHECK (dropoff_lng BETWEEN -180 AND 180),
    dropoff_note                text,
    diet                        text[] NOT NULL DEFAULT '{}',
    avoid                       text[] NOT NULL DEFAULT '{}',
    favorite_cuisines           text[] NOT NULL DEFAULT '{}',
    favorite_restaurant_ids     text[] NOT NULL DEFAULT '{}',
    -- UsualTime[]: [{label, days, start, end}], read and written whole.
    usual_times                 jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(usual_times) = 'array'),
    walking_pace                text NOT NULL DEFAULT 'normal' CHECK (walking_pace IN ('relaxed', 'normal', 'brisk')),
    runner_opt_in               boolean NOT NULL DEFAULT false,
    default_tip_cents           bigint NOT NULL DEFAULT 0 CHECK (default_tip_cents >= 0),
    notify_push                 boolean NOT NULL DEFAULT true,
    notify_messages             boolean NOT NULL DEFAULT true,
    share_location_when_running boolean NOT NULL DEFAULT false,
    CHECK ((dropoff_label IS NULL) = (dropoff_lat IS NULL) AND (dropoff_lat IS NULL) = (dropoff_lng IS NULL)),
    CHECK (dropoff_label IS NOT NULL OR (dropoff_place_id IS NULL AND dropoff_note IS NULL))
);
ALTER TABLE ferry.profiles ENABLE ROW LEVEL SECURITY;
CREATE INDEX profiles_dropoff_place_id_idx ON ferry.profiles (dropoff_place_id) WHERE dropoff_place_id IS NOT NULL;

-- Agent-texts-first sign-in (contract v1.2). POST /v1/auth/text/start {phone} makes a row and the agent
-- texts a 6-digit code to phone_e164; POST /v1/auth/text/{challenge_id}/verify {code} checks it against
-- code_hash; GET /v1/auth/text/{challenge_id} reads status. The id is the opaque challenge_id; the
-- contract lists no prefix. Only the code's hash is stored, in whatever format the auth task picks, so
-- it has no format CHECK. attempts counts wrong codes (5 expire the challenge). The token is issued
-- once verified, never stored. phone_e164 has no foreign key: the user may not exist until verify.
CREATE TABLE ferry.verify_challenges (
    id          text PRIMARY KEY,
    phone_e164  text NOT NULL CHECK (phone_e164 ~ '^\+[1-9][0-9]{1,14}$'),
    code_hash   text NOT NULL,
    attempts    integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    status      text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'verified', 'expired')),
    user_id     text REFERENCES ferry.users (id) ON DELETE CASCADE,
    is_new      boolean,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    verified_at timestamptz,
    CHECK ((status = 'verified') = (user_id IS NOT NULL AND is_new IS NOT NULL AND verified_at IS NOT NULL))
);
ALTER TABLE ferry.verify_challenges ENABLE ROW LEVEL SECURITY;
-- The rate limit: the codes sent to a phone in the last 10 minutes (at most 3).
CREATE INDEX verify_challenges_phone_e164_created_at_idx ON ferry.verify_challenges (phone_e164, created_at DESC);
CREATE INDEX verify_challenges_user_id_idx ON ferry.verify_challenges (user_id) WHERE user_id IS NOT NULL;

-- Circle. members come from circle_members; stats (runs, co2_saved_g) are derived from runs and
-- ferry.impact_daily. last_met_at and last_active_at are stored because the app sets them.
CREATE TABLE ferry.circles (
    id             text PRIMARY KEY CHECK (starts_with(id, 'cir_')),
    name           text NOT NULL,
    emoji          text NOT NULL,
    last_met_at    timestamptz,
    last_active_at timestamptz NOT NULL DEFAULT now(),
    created_at     timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE ferry.circles ENABLE ROW LEVEL SECURITY;

CREATE TABLE ferry.circle_members (
    circle_id text NOT NULL REFERENCES ferry.circles (id) ON DELETE CASCADE,
    user_id   text NOT NULL REFERENCES ferry.users (id) ON DELETE CASCADE,
    joined_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (circle_id, user_id)
);
ALTER TABLE ferry.circle_members ENABLE ROW LEVEL SECURITY;
CREATE INDEX circle_members_user_id_idx ON ferry.circle_members (user_id);

-- Area (center is center_lat, center_lng).
CREATE TABLE ferry.areas (
    id         text PRIMARY KEY CHECK (starts_with(id, 'area_')),
    name       text NOT NULL,
    center_lat double precision NOT NULL CHECK (center_lat BETWEEN -90 AND 90),
    center_lng double precision NOT NULL CHECK (center_lng BETWEEN -180 AND 180)
);
ALTER TABLE ferry.areas ENABLE ROW LEVEL SECURITY;

-- Restaurant, plus Menu.prices_are_estimates. walk_minutes_from_dropoff is derived per viewer.
-- is_open and hours_today are stored as the fixtures give them (every restaurant open, so the demo
-- works at any hour). The Nessie merchant is in ferry.nessie_links.
CREATE TABLE ferry.restaurants (
    id                   text PRIMARY KEY CHECK (starts_with(id, 'rst_')),
    name                 text NOT NULL,
    area_id              text NOT NULL REFERENCES ferry.areas (id) ON DELETE RESTRICT,
    cuisine              text[] NOT NULL DEFAULT '{}',
    lat                  double precision NOT NULL CHECK (lat BETWEEN -90 AND 90),
    lng                  double precision NOT NULL CHECK (lng BETWEEN -180 AND 180),
    address              text NOT NULL,
    is_open              boolean NOT NULL DEFAULT true,
    hours_today          text NOT NULL,
    partner              boolean NOT NULL DEFAULT false,
    price_level          smallint NOT NULL CHECK (price_level IN (1, 2, 3)),
    image_url            text,
    prices_are_estimates boolean NOT NULL DEFAULT true
);
ALTER TABLE ferry.restaurants ENABLE ROW LEVEL SECURITY;
CREATE INDEX restaurants_area_id_idx ON ferry.restaurants (area_id);

-- MenuCategory. The id is opaque: the contract lists no prefix (fixtures use cat_<restaurant>_<slug>).
CREATE TABLE ferry.menu_categories (
    id            text PRIMARY KEY,
    restaurant_id text NOT NULL REFERENCES ferry.restaurants (id) ON DELETE CASCADE,
    name          text NOT NULL,
    sort_order    integer NOT NULL DEFAULT 0,
    -- Target of menu_items' composite key, so an item's category is always its restaurant's.
    UNIQUE (id, restaurant_id)
);
ALTER TABLE ferry.menu_categories ENABLE ROW LEVEL SECURITY;
CREATE INDEX menu_categories_restaurant_id_idx ON ferry.menu_categories (restaurant_id, sort_order);

-- MenuItem.
CREATE TABLE ferry.menu_items (
    id            text PRIMARY KEY CHECK (starts_with(id, 'itm_')),
    restaurant_id text NOT NULL REFERENCES ferry.restaurants (id) ON DELETE CASCADE,
    category_id   text NOT NULL,
    name          text NOT NULL,
    description   text NOT NULL DEFAULT '',
    price_cents   bigint NOT NULL CHECK (price_cents >= 0),
    tags          text[] NOT NULL DEFAULT '{}',
    image_url     text,
    sort_order    integer NOT NULL DEFAULT 0,
    FOREIGN KEY (category_id, restaurant_id) REFERENCES ferry.menu_categories (id, restaurant_id) ON DELETE CASCADE
);
ALTER TABLE ferry.menu_items ENABLE ROW LEVEL SECURITY;
CREATE INDEX menu_items_restaurant_id_idx ON ferry.menu_items (restaurant_id, sort_order);
CREATE INDEX menu_items_category_id_idx ON ferry.menu_items (category_id, restaurant_id);

-- MenuOption (min and max are min_choices and max_choices).
CREATE TABLE ferry.item_options (
    id           text PRIMARY KEY CHECK (starts_with(id, 'opt_')),
    menu_item_id text NOT NULL REFERENCES ferry.menu_items (id) ON DELETE CASCADE,
    name         text NOT NULL,
    min_choices  integer NOT NULL DEFAULT 0,
    max_choices  integer NOT NULL DEFAULT 1,
    sort_order   integer NOT NULL DEFAULT 0,
    CHECK (min_choices >= 0 AND max_choices >= 1 AND max_choices >= min_choices)
);
ALTER TABLE ferry.item_options ENABLE ROW LEVEL SECURITY;
CREATE INDEX item_options_menu_item_id_idx ON ferry.item_options (menu_item_id, sort_order);

-- MenuChoice. price_delta_cents may be negative, as the contract allows: a "half size" choice can be a
-- discount. A line's unit_price_cents (item price plus deltas) stays >= 0 on ferry.order_lines.
CREATE TABLE ferry.item_choices (
    id                text PRIMARY KEY CHECK (starts_with(id, 'cho_')),
    option_id         text NOT NULL REFERENCES ferry.item_options (id) ON DELETE CASCADE,
    name              text NOT NULL,
    price_delta_cents bigint NOT NULL DEFAULT 0,
    sort_order        integer NOT NULL DEFAULT 0
);
ALTER TABLE ferry.item_choices ENABLE ROW LEVEL SECURITY;
CREATE INDEX item_choices_option_id_idx ON ferry.item_choices (option_id, sort_order);
