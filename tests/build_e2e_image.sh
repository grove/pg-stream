#!/usr/bin/env bash
# =============================================================================
# Build the Docker image for pg_trickle E2E integration tests.
#
# This script builds a multi-stage Docker image that:
#   1. Compiles the extension from source (Rust + cargo-pgrx)
#   2. Installs it into a clean postgres:18.1 image
#
# The resulting image can be used by testcontainers-rs in the E2E tests.
#
# Usage:
#   ./tests/build_e2e_image.sh            # default build
#   ./tests/build_e2e_image.sh --no-cache # force full rebuild
# =============================================================================
set -euo pipefail

IMAGE_NAME="pg_trickle_e2e"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ── Compute a stable, collision-free image tag from the git SHA ──────────
# Using the short SHA means concurrent builds on different branches or
# commits produce independent tags, eliminating tag-clobber races.
# A "-dirty" suffix marks images built from an uncommitted working tree so
# they never overwrite a clean-SHA image.
GIT_SHA=$(git -C "${PROJECT_ROOT}" rev-parse --short HEAD 2>/dev/null || echo "unknown")
DIRTY_COUNT=$(git -C "${PROJECT_ROOT}" status --porcelain 2>/dev/null | wc -l | tr -d ' ')
if [[ "${DIRTY_COUNT}" -gt 0 ]]; then
    GIT_SHA="${GIT_SHA}-dirty"
fi

IMAGE_TAG="${GIT_SHA}"
IMAGE_REF="${IMAGE_NAME}:${IMAGE_TAG}"
TAG_FILE="${PROJECT_ROOT}/.e2e-image-tag"

# Pass through any extra args (e.g. --no-cache)
EXTRA_ARGS="${*:-}"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Building E2E test image: ${IMAGE_REF}"
echo "  Project root: ${PROJECT_ROOT}"
echo "  Dockerfile:   ${SCRIPT_DIR}/Dockerfile.e2e"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

docker build \
    -t "${IMAGE_REF}" \
    -f "${SCRIPT_DIR}/Dockerfile.e2e" \
    ${EXTRA_ARGS} \
    "${PROJECT_ROOT}"

# Write the full image reference to the tag file so downstream targets
# (test-e2e-fast, build-upgrade-image, etc.) pick up the exact tag without
# rebuilding.
echo "${IMAGE_REF}" > "${TAG_FILE}"
echo "  Tag file written: .e2e-image-tag → ${IMAGE_REF}"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  ✓ Image built: ${IMAGE_REF}"
IMAGE_SIZE=$(docker image inspect "${IMAGE_REF}" \
    --format='{{.Size}}' 2>/dev/null || echo "0")
if command -v numfmt &>/dev/null; then
    echo "  Image size: $(echo "${IMAGE_SIZE}" | numfmt --to=iec)"
elif command -v awk &>/dev/null; then
    echo "  Image size: $(echo "${IMAGE_SIZE}" | awk '{printf "%.0f MB", $1/1024/1024}')"
else
    echo "  Image size: ${IMAGE_SIZE} bytes"
fi
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "To test manually:"
echo "  docker run --rm -d --name pgs-e2e -e POSTGRES_PASSWORD=postgres -p 15432:5432 ${IMAGE_REF}"
echo "  sleep 3"
echo "  psql -h localhost -p 15432 -U postgres -c \"CREATE EXTENSION pg_trickle;\""
echo "  docker stop pgs-e2e"
