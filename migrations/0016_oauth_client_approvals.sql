-- Which OAuth clients a user has approved before.
--
-- Dynamic Client Registration is open (any caller can register a client and choose its display
-- name), and every approval mints a full-privilege token. The consent page therefore cannot rely
-- on the client's name to convey identity: an attacker registers "Claude", points it at the real
-- claude.ai callback, and sends the authorize URL to an operator, whose consent page is
-- indistinguishable from the legitimate one.
--
-- This table is what makes a look-alike visible: an approval the user has never granted before is
-- marked as new on the consent page. It is a UI signal, not an authorization decision — the code
-- is still bound to the session user either way.
CREATE TABLE oauth_client_approvals (
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    client_id   TEXT        NOT NULL,
    approved_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    PRIMARY KEY (user_id, client_id)
);
