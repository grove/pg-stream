-- pg_trickle 0.96.0 -> 0.97.0
-- Operational assurance and monitoring assets are shipped with the extension
-- package. Keep the catalog version explicit for upgraded installations.

INSERT INTO pgtrickle.pgt_schema_version (version, description)
VALUES ('0.97.0', 'Operational assurance, monitoring contracts, and release evidence')
ON CONFLICT (version) DO NOTHING;
