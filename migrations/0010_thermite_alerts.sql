-- Alert delivery state on the notifications outbox.
--
-- The same row serves two consumers with two columns: agents claim work through the lease
-- (claimed_at/lease_until/acked_at), and the alert loop delivers to humans through alerted_at.
-- They are independent on purpose — an agent acking its triage work says nothing about whether a
-- human was told.
alter table notifications
    add column alerted_at timestamptz;

-- Drives the delivery poll: un-alerted rows are the small minority.
create index notifications_unalerted_idx
    on notifications (created_at)
    where alerted_at is null;
