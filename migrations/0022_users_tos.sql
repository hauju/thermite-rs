-- Terms-of-service acceptance, per user.
--
-- dx-auth asks for acceptance after login whenever the store reports none, or an older version
-- than its `TOS_VERSION`. Until now the store threw the acceptance away, so the question came
-- back on every login. NULL means never accepted.
ALTER TABLE users
    ADD COLUMN tos_version     TEXT,
    ADD COLUMN tos_accepted_at TIMESTAMPTZ;
