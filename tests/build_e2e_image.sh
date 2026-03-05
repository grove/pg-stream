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
# Build speed:
#   On first call, the pre-built builder base image pg_trickle_builder:pg18
#   is built automatically (Rust + cargo-pgrx + pgrx init, ~7 min once).
#   Subsequent calls to this script (without --no-cache) skip the builder
#   image step and take ~2-3 min cold / ~30 s warm.
#
# Usage:
#   ./tests/build_e2e_image.sh              # default build (auto-builds builder)
#   ./tests/build_e2e_image.sh --no-cache   # force full rebuild of both images
# =============================================================================
set -euo pipefail

IMAGE_NAME="pg_trickle_e2e"
IMAGE_TAG="latest"
BUILDER_IMAGE="pg_trickle_builder:pg18"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Pass through any extra args (e.g. --no-cache)
EXTRA_ARGS="${*:-}"

# ── Ensure the builder base image is available ───────────────────────────────
# Skip this check when --no-cache is given (the caller wants a full scratch
# build, so we don't want to rebuild the builder separately first).
if [[ "${EXTRA_ARGS}" != *"--no-cache"* ]]; then
    if ! docker image inspect "${BUILDER_IMAGE}" &>/dev/null; then
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "  Builder image not found: ${BUILDER_IMAGE}"
        echo "  Building it now (one-time, ~7 min) …"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        docker build \
            -t "${BUILDER_IMAGE}" \
            -f "${SCRIPT_DIR}/Dockerfile.builder" \
            "${PROJECT_ROOT}"
    else
        echo "  Builder image present: ${BUILDER_IMAGE}"
    fi
fi

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Building E2E test image: ${IMAGE_NAME}:${IMAGE_TAG}"
echo "  Project root: ${PROJECT_ROOT}"
echo "  Dockerfile:   ${SCRIPT_DIR}/Dockerfile.e2e"
echo "  Builder image: ${BUILDER_IMAGE}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

docker build \
    -t "${IMAGE_NAME}:${IMAGE_TAG}" \
    -f "${SCRIPT_DIR}/Dockerfile.e2e" \
    --build-arg "BUILDER_IMAGE=${BUILDER_IMAGE}" \
    ${EXTRA_ARGS} \
    "${PROJECT_ROOT}"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  ✓ Image built: ${IMAGE_NAME}:${IMAGE_TAG}"
IMAGE_SIZE=$(docker image inspect "${IMAGE_NAME}:${IMAGE_TAG}" \
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
echo "  docker run --rm -d --name pgs-e2e -e POSTGRES_PASSWORD=postgres -p 15432:5432 ${IMAGE_NAME}:${IMAGE_TAG}"
echo "  sleep 3"
echo "  psql -h localhost -p 15432 -U postgres -c \"CREATE EXTENSION pg_trickle;\""
echo "  docker stop pgs-e2e"
