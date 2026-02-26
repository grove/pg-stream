# =============================================================================
# Production Dockerfile for pg_stream — CloudNativePG compatible.
#
# This image is designed for use with the CloudNativePG Kubernetes operator.
# It contains a clean PostgreSQL 18.1 base image with the pg_stream
# extension pre-installed (shared library, control file, SQL migrations).
#
# Unlike tests/Dockerfile.e2e, this image does NOT bake in
# shared_preload_libraries — CNPG manages PostgreSQL configuration
# declaratively via the Cluster CustomResource. Users must configure
# shared_preload_libraries in their Cluster spec.
#
# Usage:
#   docker build -t pg_stream:latest -f cnpg/Dockerfile .
#
# The build context must be the project root.
# =============================================================================

# ── Stage 1: Build the extension ────────────────────────────────────────────
FROM postgres:18.1 AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
        curl \
        build-essential \
        libreadline-dev \
        zlib1g-dev \
        pkg-config \
        libssl-dev \
        libclang-dev \
        clang \
        postgresql-server-dev-18 \
        ca-certificates \
        git \
    && rm -rf /var/lib/apt/lists/*

# Install Rust toolchain (stable)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable
ENV PATH="/root/.cargo/bin:${PATH}"

# Install cargo-pgrx (must match the version in Cargo.toml)
RUN cargo install --locked cargo-pgrx --version 0.17.0

# Initialize pgrx for PG 18 using the system pg_config
RUN cargo pgrx init --pg18 /usr/bin/pg_config

# ── Dependency caching layer ────────────────────────────────────────────────
WORKDIR /build
COPY Cargo.toml ./

RUN mkdir -p src/bin src/ivm/operators benches && \
    echo '#![allow(warnings)] fn main() {}' > src/bin/pgrx_embed.rs && \
    echo '#![allow(warnings)]' > src/lib.rs && \
    echo '' > src/ivm/mod.rs && \
    echo '' > src/ivm/operators/mod.rs && \
    echo 'fn main() {}' > benches/refresh_bench.rs && \
    echo 'fn main() {}' > benches/diff_operators.rs && \
    cargo generate-lockfile && \
    cargo fetch

# ── Build the real extension ────────────────────────────────────────────────
COPY src/ src/
COPY pg_stream.control ./

RUN cargo pgrx package --pg-config /usr/bin/pg_config

# Verify artifacts
RUN echo "=== Package artifacts ===" && \
    find target/release/pg_stream-pg18 -type f && \
    echo "========================="

# ── Stage 2: Production runtime image ──────────────────────────────────────
FROM postgres:18.1

# OCI image labels — REPO_URL is injected by the release workflow so the image
# is linked to the exact GitHub repository. Override at build time for forks:
#   docker build --build-arg REPO_URL=https://github.com/you/pg-stream ...
ARG REPO_URL=https://github.com/grove/pg-stream

# CloudNativePG's operator always runs initdb (and the postgres process) as
# UID 26 — the RHEL/CentOS convention for the 'postgres' system account.
# The official postgres:18.1 Debian image uses UID/GID 999, so UID 26 does not
# exist in /etc/passwd and initdb fails with:
#   "could not look up effective user ID 26: user does not exist"
#
# Fix: remap the 'postgres' account to UID/GID 26 and re-own its home/socket
# directories. The data directory itself is a mounted volume so no rechown
# of /pgdata is needed; CNPG will own that via the PVC.
#
# The groupmod/usermod commands must be idempotent: if GID/UID 26 is already
# claimed by another account we remove that account first; if 'postgres'
# already owns 26 we skip the step.
RUN set -eux; \
    cur_gid=$(getent group postgres | cut -d: -f3); \
    cur_uid=$(id -u postgres); \
    # ── Fix GID ── \
    if [ "$cur_gid" != "26" ]; then \
        # If another group already holds GID 26, delete it first \
        existing=$(getent group 26 | cut -d: -f1 || true); \
        if [ -n "$existing" ] && [ "$existing" != "postgres" ]; then \
            groupdel "$existing"; \
        fi; \
        groupmod -g 26 postgres; \
    fi; \
    # ── Fix UID ── \
    if [ "$cur_uid" != "26" ]; then \
        # If another user already holds UID 26, delete it first \
        existing=$(getent passwd 26 | cut -d: -f1 || true); \
        if [ -n "$existing" ] && [ "$existing" != "postgres" ]; then \
            userdel "$existing"; \
        fi; \
        usermod -u 26 -g 26 postgres; \
    fi; \
    chown -R 26:26 /var/lib/postgresql /var/run/postgresql

LABEL org.opencontainers.image.title="pg_stream"
LABEL org.opencontainers.image.description="PostgreSQL 18 with pg_stream extension for CloudNativePG"
LABEL org.opencontainers.image.vendor="pg_stream"
LABEL org.opencontainers.image.licenses="Apache-2.0"
LABEL org.opencontainers.image.source="${REPO_URL}"

# Copy extension shared library
COPY --from=builder \
    /build/target/release/pg_stream-pg18/usr/lib/postgresql/18/lib/ \
    /usr/lib/postgresql/18/lib/

# Copy extension control + SQL migration files
COPY --from=builder \
    /build/target/release/pg_stream-pg18/usr/share/postgresql/18/extension/ \
    /usr/share/postgresql/18/extension/

# NOTE: We intentionally do NOT set shared_preload_libraries here.
# CloudNativePG manages PostgreSQL configuration declaratively.
# Users must add the following to their Cluster spec:
#
#   postgresql:
#     shared_preload_libraries:
#       - pg_stream

# Verify extension files are in place
RUN ls -la /usr/share/postgresql/18/extension/pg_stream* && \
    ls -la /usr/lib/postgresql/18/lib/pg_stream*

EXPOSE 5432
