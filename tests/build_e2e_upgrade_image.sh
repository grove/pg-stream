#!/usr/bin/env bash
# =============================================================================
# Build the Docker image for pg_trickle upgrade E2E tests.
#
# This builds a lightweight image that layers old-version SQL files on top
# of the standard E2E image. The resulting image supports:
#   CREATE EXTENSION pg_trickle VERSION '<from>';
#   ALTER EXTENSION pg_trickle UPDATE TO '<to>';
#
# Prerequisites: the base E2E image must be built first.
#   ./tests/build_e2e_image.sh
#
# Usage:
#   ./tests/build_e2e_upgrade_image.sh                  # defaults: 0.1.3 → 0.2.0
#   ./tests/build_e2e_upgrade_image.sh 0.1.3 0.2.0     # explicit versions
#   ./tests/build_e2e_upgrade_image.sh 0.1.3 0.2.0 --no-cache
# =============================================================================
set -euo pipefail

FROM_VERSION="${1:-0.1.3}"
TO_VERSION="${2:-0.2.0}"
shift 2 2>/dev/null || true
EXTRA_ARGS="${*:-}"

IMAGE_NAME="pg_trickle_upgrade_e2e"
IMAGE_TAG="latest"
BASE_IMAGE="${PGS_E2E_BASE_IMAGE:-pg_trickle_e2e:latest}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Verify prerequisites
if ! docker image inspect "${BASE_IMAGE}" &>/dev/null; then
    echo "ERROR: Base image '${BASE_IMAGE}' not found."
    echo "       Run './tests/build_e2e_image.sh' first."
    exit 1
fi

ARCHIVE_SQL="${PROJECT_ROOT}/sql/archive/pg_trickle--${FROM_VERSION}.sql"
UPGRADE_SQL="${PROJECT_ROOT}/sql/pg_trickle--${FROM_VERSION}--${TO_VERSION}.sql"

if [[ ! -f "$ARCHIVE_SQL" ]]; then
    echo "ERROR: Archive SQL not found: ${ARCHIVE_SQL}"
    exit 1
fi
if [[ ! -f "$UPGRADE_SQL" ]]; then
    echo "ERROR: Upgrade SQL not found: ${UPGRADE_SQL}"
    exit 1
fi

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Building upgrade E2E image: ${IMAGE_NAME}:${IMAGE_TAG}"
echo "  Upgrade path: ${FROM_VERSION} → ${TO_VERSION}"
echo "  Base image:   ${BASE_IMAGE}"
echo "  Dockerfile:   ${SCRIPT_DIR}/Dockerfile.e2e-upgrade"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

docker build \
    -t "${IMAGE_NAME}:${IMAGE_TAG}" \
    --build-arg "BASE_IMAGE=${BASE_IMAGE}" \
    --build-arg "FROM_VERSION=${FROM_VERSION}" \
    --build-arg "TO_VERSION=${TO_VERSION}" \
    -f "${SCRIPT_DIR}/Dockerfile.e2e-upgrade" \
    ${EXTRA_ARGS} \
    "${PROJECT_ROOT}"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  ✓ Image built: ${IMAGE_NAME}:${IMAGE_TAG}"
echo "  Upgrade path: ${FROM_VERSION} → ${TO_VERSION}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "To test manually:"
echo "  docker run --rm -d --name pgs-upgrade -e POSTGRES_PASSWORD=postgres -p 15432:5432 ${IMAGE_NAME}:${IMAGE_TAG}"
echo "  sleep 3"
echo "  psql -h localhost -p 15432 -U postgres -c \"CREATE EXTENSION pg_trickle VERSION '${FROM_VERSION}';\""
echo "  psql -h localhost -p 15432 -U postgres -c \"ALTER EXTENSION pg_trickle UPDATE TO '${TO_VERSION}';\""
echo "  docker stop pgs-upgrade"
