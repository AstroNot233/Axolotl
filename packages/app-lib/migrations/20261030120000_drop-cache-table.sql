-- The fetched responses are kept in the derived database; they are discarded
-- once stale, so nothing here survives the move.
DROP TABLE IF EXISTS cache;
