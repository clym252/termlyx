-- Node ids are allocated as "the lowest unused id >= 1 in the account" under
-- UNIQUE (account_id, node), so the counter column from 0001 is not used.
ALTER TABLE accounts DROP COLUMN next_node;
