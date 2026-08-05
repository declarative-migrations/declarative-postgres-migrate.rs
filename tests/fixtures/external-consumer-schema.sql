create schema if not exists external_service;

create table if not exists external_service.jobs (
    id uuid primary key,
    owner_id uuid not null,
    status text not null check (status in ('queued', 'running', 'complete', 'failed')),
    payload jsonb not null default '{}'::jsonb,
    attempts integer not null default 0 check (attempts >= 0),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists external_service_jobs_owner_status_idx
    on external_service.jobs (owner_id, status, created_at desc);

create table if not exists external_service.job_events (
    id bigint generated always as identity primary key,
    job_id uuid not null references external_service.jobs(id) on delete cascade,
    event_type text not null,
    detail jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create index if not exists external_service_job_events_job_created_idx
    on external_service.job_events (job_id, created_at, id);
