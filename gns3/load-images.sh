#!/usr/bin/env bash
# Load the FRR router images into the *host* Docker daemon.
#
# GNS3 creates router nodes as sibling containers on the host daemon, so the images have to exist
# there rather than inside the gns3server container.
set -euo pipefail

TARBALL="${1:-$(dirname "$0")/../images/frr-images-min.tar.gz}"

if [[ ! -f "$TARBALL" ]]; then
    echo "error: image archive not found: $TARBALL" >&2
    echo "Pass the path explicitly: $0 /path/to/frr-images-min.tar.gz" >&2
    exit 1
fi

echo "Loading FRR images from $TARBALL (this takes a minute)..."
docker load -i "$TARBALL"

# The experiments reference these by tag; fail loudly now rather than mid-run.
echo
echo "Verifying expected images:"
missing=0
for img in frr:latest frr:10.2.1 frr:8.4.2 frr:8.5.1 frr:gns-alpine-lp-bug frr:gns-alpine-mrai-bug gns3/ipterm:latest; do
    if docker image inspect "$img" >/dev/null 2>&1; then
        echo "  ok      $img"
    else
        echo "  MISSING $img" >&2
        missing=1
    fi
done

if [[ $missing -ne 0 ]]; then
    echo >&2
    echo "error: some expected images are missing; the experiments will fail." >&2
    exit 1
fi

echo
echo "All expected images present."
