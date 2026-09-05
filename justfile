clean:
    cargo clean

fmt:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Mirrors the CI test jobs: the app crate needs the server feature selected
# explicitly, since its default feature builds the wasm client.
test:
    cargo test --workspace --exclude thermite
    cargo test -p thermite --no-default-features --features server

check: fmt clippy test

# Regenerate the committed sqlx query metadata after changing SQL or migrations.
# Needs a running database (`just init`).
prepare:
    cargo sqlx prepare -- --no-default-features --features server

init:
    docker compose up -d

serve:
    dx serve --addr 0.0.0.0

tw:
    bunx @tailwindcss/cli -i tailwind.css -o ./assets/tailwind.css

# Re-render the favicon and PWA icons from their SVG sources after changing the
# mark. Needs librsvg and ImageMagick (`brew install librsvg imagemagick`).
icons:
    #!/usr/bin/env bash
    set -euo pipefail
    rsvg-convert -w 512 -h 512 assets/pwa/icon.svg -o assets/pwa/icon-512.png
    rsvg-convert -w 192 -h 192 assets/pwa/icon.svg -o assets/pwa/icon-192.png
    rsvg-convert -w 180 -h 180 assets/pwa/icon.svg -o assets/pwa/apple-touch-icon.png
    rsvg-convert -w 512 -h 512 assets/pwa/icon-maskable.svg -o assets/pwa/icon-maskable-512.png
    # favicon.ico stops at 64: ImageMagick writes ICO frames as uncompressed BMP,
    # so a 256px frame alone is 270 KB, and browsers only ever reach for 16/32
    # (48 on Windows). Anything wanting HiDPI gets the SVG favicon instead.
    tmp=$(mktemp -d)
    for s in 16 32 48 64; do
        rsvg-convert -w "$s" -h "$s" assets/favicon.svg -o "$tmp/fav-$s.png"
    done
    magick "$tmp"/fav-16.png "$tmp"/fav-32.png "$tmp"/fav-48.png \
           "$tmp"/fav-64.png assets/favicon.ico
    rm -rf "$tmp"

# Copy .env.example -> .env and fill in a freshly generated SESSION_SECRET.
bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -f .env ]]; then
        echo ".env already exists; refusing to overwrite."
        exit 1
    fi
    cp .env.example .env
    SECRET=$(openssl rand -hex 64)
    perl -i -pe "s/^SESSION_SECRET=\$/SESSION_SECRET=$SECRET/" .env
    echo "Wrote .env with a fresh SESSION_SECRET."
