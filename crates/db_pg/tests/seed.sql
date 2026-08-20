-- The schema the live tests expect.
--
-- Apply it to an empty database and set `TUPLI_TEST_PG` at it:
--
--     createdb tupli_dev
--     psql -d tupli_dev -f crates/db_pg/tests/seed.sql
--
-- It is deliberately not a plain table or two: every object here exists to make
-- one introspection or rendering question answerable — an enum type, an array
-- column, jsonb, a partial index, an expression index, a materialised view in
-- another schema, a generated column, a disabled trigger, a self-referencing
-- foreign key, and comments on both a table and a column.

create schema if not exists analytics;

create type plan_tier as enum ('free', 'team', 'pro');

create table organizations (
    id         uuid primary key default gen_random_uuid(),
    name       text not null,
    created_at timestamptz not null default now()
);

create table users (
    id              bigint generated always as identity primary key,
    email           text not null unique,
    full_name       text,
    organization_id uuid not null references organizations (id) on delete cascade,
    plan            plan_tier not null default 'free',
    mrr_cents       integer not null default 0,
    is_active       boolean not null default true,
    tags            text[] not null default '{}',
    settings        jsonb not null default '{}',
    score           numeric(10, 2),
    created_at      timestamptz not null default now()
);

create index users_by_org on users (organization_id, created_at desc);
create index users_active on users (email) where is_active;

create view active_users as select * from users where is_active;

create materialized view analytics.plan_totals as
    select plan, count(*) as users, sum(mrr_cents) as mrr from users group by plan
    with no data;

create function mrr_for(org uuid) returns bigint language sql stable as
$$ select coalesce(sum(mrr_cents), 0) from users where organization_id = org $$;

-- Twenty thousand rows, so `estimated_rows` has something to estimate and the
-- grid has something to scroll.
insert into organizations (name)
    select 'org ' || n from generate_series(1, 50) as n;
insert into users (email, full_name, organization_id, plan, mrr_cents, is_active, score)
    select 'user' || n || '@example.com',
           case when n % 7 = 0 then null else 'User ' || n end,
           (select id from organizations order by name limit 1 offset (n % 50)),
           (array['free', 'team', 'pro'])[1 + n % 3]::plan_tier,
           (n % 500) * 100,
           n % 11 <> 0,
           round((n % 1000) / 10.0, 2)
      from generate_series(1, 20000) as n;
analyze;

-- The table the edit tests write to. Small, disposable, and the only one they
-- touch, so a failed commit cannot damage anything a later test reads.
create table edit_demo (
    id    serial primary key,
    email text unique,
    note  text
);

-- Everything the Structure and DDL tabs have to render, in one table.
create table structure_demo (
    id             bigint generated always as identity primary key,
    parent_id      bigint references structure_demo (id) on delete set null,
    label          text not null,
    amount_cents   integer not null default 0,
    amount_dollars numeric generated always as (amount_cents::numeric / 100.0) stored,
    created_at     timestamptz not null default now(),
    constraint structure_demo_amount_positive check (amount_cents >= 0),
    constraint structure_demo_label_present check (length(label) > 0)
);

comment on table structure_demo is 'every DDL feature the tab renders';
comment on column structure_demo.label is 'not empty; see the check';

create unique index structure_demo_root_label
    on structure_demo (lower(label))
    where parent_id is null;

create function structure_demo_touch() returns trigger language plpgsql as
$$ begin return new; end $$;

create trigger structure_demo_touch
    before update on structure_demo
    for each row execute function structure_demo_touch();

create trigger structure_demo_audit
    after insert on structure_demo
    for each statement execute function structure_demo_touch();

alter table structure_demo disable trigger structure_demo_audit;
