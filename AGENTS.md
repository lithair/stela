# Working on Stela

## Purpose and source of truth

Stela is a single-binary blog engine built on the maintainer's Lithair
framework. It also exercises the maintainer's toolchain in real use: Lithair
for runtime, cidx for CI, probatum for HTTP behaviour, and eventually
configorator for provisioning and deployment. Finding reproducible upstream
bugs is part of the project, not a distraction from it.

This guidance is distilled from `CLAUDE.md` and checked against the repository
on 2026-09-05. Consult `CLAUDE.md` for decision history, but verify its dated
status statements against code and configuration. It still contains obsolete
claims about design-only status, auth paths, dependency versions, and themes.
Keep shared architectural guidance consistent when changing either file.

The local package is version 0.0.1 and uses Lithair 1.10. The repository
describes the product as working but unreleased; verify external release
status before making claims about registries or deployments.

## Repository map

- `src/main.rs`: CLI, config/scaffolding, models, server wiring, rebuild,
  rendering, validation, and Rust unit tests.
- `theme/`: embedded Tera templates and CSS, including login and editor.
- `probatum.toml`: ordered HTTP and CLI behaviour checks.
- `tests/asset_lifecycle.py`: probatum's Python standard-library helper for
  PUT/DELETE and restarts; `tests/probatum-delete.toml` is a separate known-red
  model deletion regression. See `tests/README.md` for invocation details.
- `e2e/editor.spec.js`, `playwright.config.js`: browser interaction tests.
- `cidx.toml`, `.cidx/presets.toml`: pipeline and project container presets.
- `.github/workflows/cidx.yml`: generated workflow; change its source config
  and regenerate instead of applying hand patches.
- `Dockerfile`, `docker-compose.yml`: static musl build and scratch image.

## Architectural constraints

- Ship one binary: no external database, Node runtime, external site generator,
  or separate frontend build pipeline. Node is a browser-test dependency only.
- Render public pages in process on startup and on writes using Tera 1.x and
  pulldown-cmark. Currently each rebuild renders the whole published site.
  Serve finished assets through Lithair's `FrontendServer` and SCC2 memory;
  specify MIME types with `update_asset_with_mime` for extensionless URLs.
- Models are `Post`, `Page`, and `SiteSettings`, derived with
  `DeclarativeModel`. Slugs are post/page primary keys. Posts use
  `/posts/{slug}`, pages use `/{slug}`, and settings use the key `site`.
  Pages do not belong in the RSS feed.
- Lithair's disk event store under `data/` is the durable content source.
  Rebuild currently uses throwaway model handlers to replay it. Avoid adding
  another writer to the same store or assuming replay is safe concurrently.
- Keep explicit `POST <admin-prefix>/rebuild` for deterministic publication
  from the editor, alongside `on_mutation` for API clients. Mutation callbacks
  only signal a capacity-one channel using `try_send`; the worker rebuilds.
  The worker and explicit route share a mutex to prevent overlapping replays.
- Reconcile persisted post/page assets against the published model paths on
  every rebuild, including startup. Lithair 1.10's `delete_asset` persists
  deletion tombstones. Preserve built-in assets and other URL namespaces.
- Render the editor per request behind authentication. Never store the editor
  or drafts in the public asset engine.
- Templates and CSS are currently compiled in with `include_str!`; changes
  require recompilation. Loading a theme folder on rebuild is a design goal,
  not an implemented capability. Zola is a source of ideas and themes, never
  a dependency; do not promise Zola-theme compatibility.
- Presentation settings override config fallbacks on rebuild. `stela.toml`
  supplies identity/access and initial presentation. Do not assume uniform
  CLI precedence: explicit admin route/user override config, while config
  currently wins over title/description/base-url flags and password env.

## Security invariants

- Use Lithair's sessions, RBAC, route guards, firewall, dashboard, data admin,
  and Argon2 implementation. Investigate framework gaps before duplicating
  these facilities in Stela.
- Mount editor, login/logout/validate, rebuild, dashboard (`/panel`), and
  data admin (`/data`) under one installation-specific prefix. Call
  `with_auth_path` before `with_rbac_config`, which registers auth routes.
  Only the login is exempt from the prefix's session guard. Public blog
  assets remain anonymously readable.
- The random prefix is defence in depth, not authentication. Keep session
  checks mandatory, including `with_models_require_session(true)` on models.
  Never make drafts public to enable headless reads.
- `stela new` generates a route and password using OS randomness, prints the
  credentials once, and stores only an Argon2id hash. It accepts an empty
  existing directory and refuses a nonempty directory or existing config.
  Without config, serving requires an admin route and
  `STELA_ADMIN_PASSWORD`; never introduce a plaintext password flag/default.
- Preserve generic login failures, Argon2 verification, and login-scoped
  rate limiting. Argon2's computational cost is not a concurrency limit.
- Escape raw Markdown HTML. Validate slugs and admin routes before using
  them in asset paths or JavaScript. Keep `</` escaped in embedded post JSON.
- Escaping depends on output context: Tera HTML escaping is unsuitable inside
  script literals. Existing route `| safe` relies on strict validation. Do
  not generalize it to unvalidated URLs; RSS still needs XML escaping.

## Validation and commands

Assign each assertion to the appropriate layer instead of duplicating it:
Rust unit tests for pure functions, probatum for CLI/HTTP behaviour,
Playwright for browser JavaScript and human interactions. cidx orchestrates
these and owns quality/security checks.

```bash
cargo build --locked
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo run -- --help

cidx validate
cidx run code
cidx run ci
cidx generate github -o .github/workflows/cidx.yml --force

cargo build --locked --release --target x86_64-unknown-linux-musl
probatum run
npx playwright test
```

The last two commands need the musl release artifact. `probatum run` also
needs Python 3 for the lifecycle helper. In cidx, `probatum-runner` exports
the static runner before `probatum` runs the suite in a Python Alpine image.
Both are test-only containers; no Python is shipped in Stela.
The musl build needs a
musl C toolchain for aws-lc-rs; CI uses `clux/muslrust:1.95.0-stable`.
Playwright needs installed npm dependencies and matching browsers (currently
1.56.0 in both package and container). cidx needs a working container runtime.

Prefer the configured ephemeral CI containers for full integration runs.
`probatum.toml` assumes `/work` and fixed `/tmp/blog-*` paths; it is not a
portable, repeatable host command as written. Playwright deletes its fixed
`/tmp/e2e-blog` directory on startup. For ad hoc probes, use a unique temporary
directory and an unused loopback port; never use a real blog's data.
Use `127.0.0.1` rather than relying on localhost IPv4/IPv6 resolution.
Report checks actually run and any environment limitations explicitly.

The scratch image has no shell. Debug the binary on a host, or use a temporary
debug image. Containers must bind `0.0.0.0` and use a numeric UID/GID matching
the bind mount owner. Preserve the static musl shipping target.

## Development and upstream workflow

- Rust edition 2021, MIT OR Apache-2.0, `log` for application logging;
  `println!` is reserved for CLI output. Stela-specific env uses `STELA_`;
  Lithair configuration uses `LT_`.
- Use feature branches, conventional commits, PRs, and squash merges. Read
  automated review comments before merging. Preserve unrelated local work.
- Consume Lithair from crates.io. A temporary local `[patch.crates-io]` is
  acceptable for diagnosis but must never be committed. Locate the actual
  sibling checkout instead of trusting historical paths.
- When an integration fails, capture the trigger, expected/actual behaviour,
  resolved versions, and a minimal reproduction; determine which project
  owns the fix. Keep a Stela regression at the appropriate test layer.
- Draft upstream issues with evidence for review before posting; do not send
  issues or other external messages without user authorization. Remove local
  workarounds once the upstream fix is consumed.
- Keep cidx scanner exclusions for generated `.cargo/`, `target/`, and
  `node_modules/`; dependency caches contain unrelated lockfiles. Diagnose
  the scanned path before treating a report as a Stela dependency finding.
- Configorator adoption comes after the usable blog, not as an MVP gate.
  Do not expand into comments, uploads/media library, plugins, multi-author,
  theme switching, or a second frontend stack without a concrete need.

## Known gaps to verify before extending

See `AUDIT.md` for the initial local audit and reproductions. In particular:

- Depublishing is fixed using Lithair 1.10.0's asset deletion and Stela's
  reconciliation (https://github.com/lithair/lithair/issues/227). However,
  model DELETE still resurrects posts/pages on replay in Lithair 1.10.0:
  `DeclarativeHttpHandler::replay_events` inserts Deleted event payloads.
  Do not claim model deletion works because asset deletion does.
- The catch-all GET shadows model reads: authenticated `GET /api/posts`
  returns 404. Resolve routing and published-versus-draft access together.
- RSS URLs use `| safe` with an unvalidated `base_url`, allowing malformed XML.
- Folder themes, an integrated dashboard/editor tab, and presentation editing
  in the Stela editor are not implemented despite some historical wording.
