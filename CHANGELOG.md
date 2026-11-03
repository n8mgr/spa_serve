# Changelog
## 0.1.0 (2026-11-03)

### Breaking Changes

#### Publish multi-platform images to `ghcr.io/n8mgr/spa_serve` with automated release PRs and versioned tags.

Replace the Go server with Rust and Tokio. Fix concurrent index responses, restrict file access to the web root, handle file errors and directories correctly, reject unsupported HTTP methods, validate startup arguments, and retain byte ranges, Last-Modified responses, and HTTP header/idle timeouts.
