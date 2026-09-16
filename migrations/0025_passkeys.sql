-- Passkey (WebAuthn) credentials, for the local sign-in mode where thermite is its own Relying
-- Party (the RP ID is BASE_URL's host). With no identity provider there is nowhere else for them
-- to live, and an account created by the admin password or an emailed code has no IdP token with
-- which it could ever enroll one upstream.
--
-- `credential_id` is the authenticator-chosen id, base64url-encoded; the spec makes it globally
-- unique, which is what lets the autofill login path resolve an account from an assertion alone,
-- before any address is typed.
--
-- `public_key` is the raw COSE key captured at registration. `sign_count` is the authenticator's
-- signature counter, which must never regress for a credential; 0 means the authenticator keeps
-- none, which is normal for synced platform passkeys. `backed_up` mirrors the BS flag from the
-- last assertion.
create table user_passkeys (
    id            uuid        primary key default uuid_generate_v4(),
    user_id       uuid        not null references users (id) on delete cascade,
    credential_id text        not null unique,
    public_key    bytea       not null,
    sign_count    bigint      not null default 0,
    transports    jsonb       not null default '[]'::jsonb,
    name          text        not null default '',
    backed_up     boolean     not null default false,
    created_at    timestamptz not null default now(),
    last_used_at  timestamptz
);

-- Listing a user's credentials is what the login path does to decide between the passkey prompt
-- and the password or emailed-code ones.
create index user_passkeys_user_id_idx on user_passkeys (user_id);
