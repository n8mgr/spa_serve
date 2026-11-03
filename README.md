# spa_serve

A minimal binary dedicated to serving static single-page applications. Copy your
build output into `/www`; it serves assets on port 80 and falls back to
`index.html` for client-side routing. No server configuration required.

## Why this image

- Stripped executable in a `scratch` image; no shell or JavaScript runtime.
- Built-in SPA fallback; copy in your build output and run.
- GET, HEAD, byte ranges, ETags, and Last-Modified support.
- File access confined to the web root, including symlink targets.

| Image | amd64 compressed | arm64 compressed |
| --- | ---: | ---: |
| spa_serve | 0.94 MB | 0.89 MB |
| [nginx:alpine](https://hub.docker.com/v2/repositories/library/nginx/tags/alpine) | 26.33 MB | 26.26 MB |
| [caddy:alpine](https://hub.docker.com/v2/repositories/library/caddy/tags/alpine) | 24.87 MB | 23.63 MB |

## Usage

Build your application and copy its output into the image:

```dockerfile
# build
FROM node:lts AS build

WORKDIR /app

COPY . .
RUN npm install && npm run build

FROM ghcr.io/n8mgr/spa_serve:latest

COPY --from=build /app/dist /www
```

Use your project's output directory if it differs from `dist`. To serve an
existing local build directly:

```sh
docker run --rm -p 8080:80 \
  -v "$PWD/dist:/www:ro" \
  ghcr.io/n8mgr/spa_serve:latest
```

Open `http://localhost:8080`. The default command is `spa_serve /www`, and
`/www/index.html` must exist when the server starts.

## Routing

- Existing files are served directly.
- Missing paths, including missing assets, return the root `index.html`, not 404.
- Directory requests return the root `index.html`, not per-directory indexes.
- Only GET and HEAD are accepted; other methods return 405.
- Static builds only; no SSR, API handlers, or Node.js runtime.

## Image tags

Images support Linux amd64 and arm64. `latest` tracks the latest release;
`master` tracks the default branch and is rebuilt daily. Builds also publish
short commit SHA tags, and releases publish full version tags such as `0.1.0`.

## Development

The server uses Rust and Tokio. Development requires Rust 1.99 or newer; the
Docker build uses Rust 1.99.1. Run `cargo test --locked --all-features`,
`cargo clippy --locked --all-targets --all-features -- -D warnings`, and
`cargo +nightly fmt --all -- --check` before submitting changes.

## Releases and automation

Releases use [Knope](https://knope.tech/) and follow the change-file and release-PR
pattern used by [Sia's release-slack-bot](https://github.com/SiaFoundation/release-slack-bot/blob/master/knope.toml),
versioning both `Cargo.toml` and `Cargo.lock`.
Run `knope document-change` to add a change file to your PR. Pushes to `master`
with pending change files create or update the `release` PR. Merge that PR with
its `chore: prepare release` title intact to publish the versioned images and
GitHub release after tests pass.

The release workflow follows [Sia's release-slack-bot](https://github.com/SiaFoundation/release-slack-bot/blob/master/.github/workflows/release.yml):
publish the branch/SHA image, create the Knope release, then retag that exact
image digest as `latest` and the released version. Rust CI follows
[the same repo's CI](https://github.com/SiaFoundation/release-slack-bot/blob/master/.github/workflows/main.yml):
nightly rustfmt, stable Clippy, documentation checks, and tests.

Enable **Allow GitHub Actions to create and approve pull requests** under the
repository's Actions settings. Bot-created release PRs may require approval to
run their CI workflows. Publishing uses the built-in `GITHUB_TOKEN`.
After the first publish, set the GHCR package
visibility to public if you want anonymous image pulls.

Dependabot groups weekly Cargo and Docker updates, and monthly GitHub Actions
updates. Cargo updates follow
[sia-sdk-rs](https://github.com/SiaFoundation/sia-sdk-rs/blob/master/.github/dependabot.yml);
Actions updates follow [hostd](https://github.com/SiaFoundation/hostd/blob/master/.github/dependabot.yml).
