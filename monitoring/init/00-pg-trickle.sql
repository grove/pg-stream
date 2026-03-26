-- Create pg_trickle extension in all databases on first start
-- This file mirrors the one in Dockerfile.hub and ensures the extension is
-- loaded when monitoring/init is mounted over /docker-entrypoint-initdb.d
CREATE EXTENSION IF NOT EXISTS pg_trickle CASCADE;
