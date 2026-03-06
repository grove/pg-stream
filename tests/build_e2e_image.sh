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

# ── Auto-detect build platform ───────────────────────────────────────────────
# Used only for comparison: if the existing builder image was built for a
# different architecture (e.g. a stale linux/amd64 image on Apple Silicon)
# we force a rebuild.  The build itself has NO --platform flag so Docker
# defaults to the native OS/arch and stores images in the Docker daemon's
# classic image store (required by docker run and testcontainers via bollard).
# Passing --platform on Docker Desktop 4.60+ with the containerd image store
# causes images to land in the containerd namespace, invisible to docker run.
# Override this detection by setting DOCKER_PLATFORM in the environment.
PLATFORM="${DOCKER_PLATFORM:-linux/$(uname -m | sed 's/x86_64/amd64/;s/aarch64/arm64/')}"

# ── Ensure the builder base image is available ───────────────────────────────
# Skip this check when --no-cache is given (the caller wants a full scratch
# build, so we don't want to rebuild the builder separately first).
if [[ "${EXTRA_ARGS}" != *"--no-cache"* ]]; then
    # Check both presence and platform match; force rebuild on mismatch so a
    # stale amd64 builder on an arm64 host doesn't silently fail at build time.
    BUILDER_PLATFORM=$(docker image inspect "${BUILDER_IMAGE}" \
        --format='{{.Os}}/{{.Architecture}}' 2>/dev/null || echo "")
    if [[ "${BUILDER_PLATFORM}" != "${PLATFORM}" ]]; then
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        if [[ -z "${BUILDER_PLATFORM}" ]]; then
            echo "  Builder image not found: ${BUILDER_IMAGE}"
        else
            echo "  Builder image platform mismatch: got ${BUILDER_PLATFORM}, need ${PLATFORM}"
        fi
        echo "  Building it now (one-time, ~7 min) …"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        docker build \
            -t "${BUILDER_IMAGE}" \
            -f "${SCRIPT_DIR}/Dockerfile.builder" \
            "${PROJECT_ROOT}"
    else
        echo "  Builder image present (${BUILDER_PLATFORM}): ${BUILDER_IMAGE}"
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
