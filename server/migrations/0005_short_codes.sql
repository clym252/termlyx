ALTER TABLE invites ADD COLUMN code_id BLOB;
CREATE UNIQUE INDEX invites_code_id ON invites(code_id) WHERE code_id IS NOT NULL;
CREATE TABLE setup_code (id INTEGER PRIMARY KEY CHECK (id = 1), code TEXT NOT NULL);
