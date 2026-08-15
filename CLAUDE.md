# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with
code in this repository.

## What Stela is

Single-binary blog engine built on [Lithair](https://github.com/lithair/lithair)
(the maintainer's own framework, `lithair-core` on crates.io). Pitch: the Ghost
experience without the infrastructure — no external database, no Node at
runtime, no build pipeline. Two commands (`stela new`, `stela serve`) and you
publish.

**Status: design phase.** 0.0.1 on crates.io is a name reservation only.

## Architecture decisions (settled July 2026 — don't relitigate without cause)

- **Rendering: in-process static build, on write — not per request.**
  `tera` + `pulldown-cmark` render pages inside the binary; the resulting HTML
  is pushed into Lithair's `FrontendEngine` via `update_asset()`
  (`lithair-core/src/frontend/engine.rs:423`) and served from SCC2 memory.
  Reads never touch a template engine. Chosen over an Astro/Node front (Node
  dependency, rebuild-on-publish complexity) and over per-request Tera (needs
  in-process store access on every read, which `with_model_full` does not
  hand back — see the rebuild trigger below).
- **Zola is a source of themes and syntax, never a dependency.** It cannot be
  one: getzola/zola is not published on crates.io (the `zola` crate there is an
  unrelated squat), and depending on its workspace components via git would
  make `cargo publish` impossible. Its ingredients ARE published and are what
  Stela uses: `tera` (its template engine), `pulldown-cmark`, `syntect`.
  Shelling out to a `zola` binary is rejected: it breaks single-binary install
  and reintroduces rebuild-on-publish.
- **Rebuild trigger: an explicit Stela route, no Lithair change.**
  `POST /admin/rebuild` builds a throwaway `DeclarativeModelHandler::<Post>`
  — its `new()` replays the event log from disk (`model_handler.rs:289`), so it
  sees writes made through the API — then renders and pushes every page.
  The admin's "publish" button calls `POST /api/posts` then the rebuild.
  Automatic hot-reload (write triggers rebuild with no caller) WOULD need a
  post-commit hook upstream in Lithair, which does not exist (`LifecycleAware`
  is field policies only). That hook is a v0.2 comfort, never an MVP blocker.
  VERIFIED 2026-07-27: the event store does flush synchronously on write. A
  POST to `/api/posts` followed by `POST /admin/rebuild` renders the new post —
  the throwaway handler's replay sees it. This was the architecture's main risk;
  it is settled, and the probatum checks keep it that way.
- **Pages are served by Lithair's `FrontendServer`, straight from SCC2 memory.**
  Stela hand-rolled this for two releases and no longer does. `update_asset_with_mime`
  (1.4, [lithair#193](https://github.com/lithair/lithair/issues/193)) makes the
  content type travel with the asset, which matters because a blog's URLs have
  no extension for detection to work from; and a miss now returns the theme's
  `/404.html` (1.6, [lithair#206](https://github.com/lithair/lithair/pull/206))
  instead of the framework's built-in page. Both gaps were found here and fixed
  upstream. Do not reintroduce a local serving route without a reason neither
  of those covers.
- **Theme = a folder of Tera templates + CSS**, read at every rebuild (so a
  template edit needs a rebuild, not a restart). Pitch
  "Tera syntax, porting a Zola theme is reasonable work" — do NOT promise
  "Zola-theme compatible" (its built-ins `get_section`, shortcodes, taxonomies
  would each need reimplementing; add the top 2-3 only when a real port asks).
- **Content models: `Post`, `Page`, `SiteSettings`** via
  `#[derive(DeclarativeModel)]`. Public read / RBAC write is the canonical
  Lithair pattern (see lithair's rbac-session example). `SiteSettings` feeds
  the Tera context, so admin edits (title, colors, menus) apply on the next
  rebuild — which the admin triggers as part of saving.
- **Admin lives at a per-install random route** — `/secure-xxxxxxx`, generated
  once by `stela new` and printed for the user to write down. This is
  defense-in-depth against drive-by scanners, NOT authentication: a URL leaks
  through Referer headers, browser history, and proxy logs. The session/RBAC
  gate stays mandatory behind it. Print that caveat where the route is shown,
  so nobody treats the secret URL as the lock.
- **One admin panel, the editor is a tab in it** — not a second "edit panel".
  A second panel means a second route, a second auth surface and a second
  thing to keep in sync, to buy a separation nobody has asked for yet. Split
  only when a real need shows up.
- **Headless stays free:** the REST content API comes with DeclarativeModel;
  an Astro/other front consuming it is an advanced path, never the default.
- **Storage:** Lithair event store on disk (`data/`), memory-first serving.
  No external DB, ever — that's the product's reason to exist.
- **The shipping artifact is a statically linked musl binary.** "One binary,
  zero infrastructure" is not satisfied by "one file": a glibc-linked binary is
  already one file, but refuses to start on any host whose glibc predates the
  build's (`GLIBC_2.xx not found`). Static-pie musl runs on any Linux, Alpine
  included, which is what "copy it to your VPS and run it" actually requires —
  and what configorator will deploy. Built by `cargo-build-musl`
  (`.cidx/presets.toml`), which needs `musl-tools`: lithair-core pulls rustls,
  whose aws-lc-rs backend has a C component. Verified against lithair-core
  1.3.0 — builds clean, static-pie, runs on bare alpine.
  Known cost: musl's allocator is slower than glibc's under thread contention.
  Unmeasured, and not worth measuring before there is a server to measure. If
  throughput ever disappoints, reach for jemalloc or mimalloc as the global
  allocator before abandoning static linking.

## MVP scope (and non-goals)

MVP: `Post` (slug, title, markdown body, published flag), `Page`,
`SiteSettings`, admin UI at the random route behind session/RBAC (markdown
editor as a tab), one default theme (index, article, page, RSS + one CSS
file), markdown via `pulldown-cmark`.

**Shipped so far:** `Post` (slug as primary key, since the slug IS the URL),
`stela serve`, the default theme compiled into the binary, markdown via
`pulldown-cmark`, `/`, `/posts/:slug`, `/rss.xml`, `POST /admin/rebuild`, and
session auth — `/auth/login`, `/auth/logout`, every write gated, the admin panel
at the operator's own route.

**Auth decisions, settled:**
- `stela serve` **refuses to start** without `--admin-route` and
  `STELA_ADMIN_PASSWORD`. No defaults: a predictable admin path is one every
  scanner knows, and an unset password would mean an open panel. The password
  comes from the environment, never a flag — arguments show up in `ps` and shell
  history.
- The password is hashed once at boot with Lithair's Argon2id
  (`lithair_core::security`), so login compares hashes rather than plaintext.
  When `stela new` arrives it should store the hash and stop handling the
  plaintext at all.
- Session tokens are 256 bits from `getrandom`. The cookie is `session_token`
  because that is the name Lithair's extractor looks for
  (`http/declarative.rs`), and it carries `HttpOnly; SameSite=Strict; Secure`.
  `Secure` is unconditional: browsers and curl both treat localhost as a secure
  context, so local development works, and anything else should be behind TLS.
- Writes are gated by `with_models_require_session(true)` for `/api/*` and
  `RouteGuard::RequireAuth` for `/admin/*` and the admin route. Public reads are
  untouched — they are assets, not model routes.
- Login failures never distinguish a bad username from a bad password, and
  Argon2 verification runs either way so the two take the same time.

**Next:** the markdown editor as a tab in the admin panel. RBAC roles beyond
"the admin is logged in" wait for a second kind of user to exist.

Explicit NON-goals for the MVP — add only when a real user asks: comments,
media library/uploads, multi-theme switching, plugins, multi-author,
Zola-theme compatibility, any second front-end stack.

## Namespace (reserved July 2026)

- crates.io: `stela` — CLI binary is also `stela`
- Domain: `stela.run`
- Repo: `github.com/lithair/stela` (under the framework org on purpose —
  showcase + single-org maintenance; GitHub transfer redirects make moving to
  the reserved `stela-run` org painless if the product outgrows it)

## Conventions (inherited from the Lithair workspace)

- Rust edition 2021, license MIT OR Apache-2.0, `log` crate for logging
  (no println!/eprintln! outside the CLI's user-facing output).
- Lithair env vars use the `LT_` prefix; the config hierarchy is
  defaults < config.toml < .env (dotenvy, loaded by the app) < env < builder.
  See lithair's docs/configuration-reference.md. Stela-specific vars: prefix
  undecided — decide when the first one appears.
- Trunk-based development: feature branches + PRs, squash merge, conventional
  commits. Read all automated review comments before merging.
- Lithair is consumed from crates.io (SemVer 1.x contract). During
  development a `[patch.crates-io]` on the local checkout
  (`../lithair/lithair-core`) is fine, but it must never be committed.

## The full-stack dogfooding intent

Stela is the first project meant to run the maintainer's ENTIRE toolchain,
one tool per lifecycle phase — integrate in this order, and never gate the
MVP on the later ones:

1. **lithair** (runtime) — ✅ WIRED, it IS the product. `lithair-core` 1.3 from
   crates.io.
2. **cidx** (CI) — ✅ WIRED, v3.2.0. `cidx.toml` plus one project preset in
   `.cidx/presets.toml`. `.github/workflows/cidx.yml` is now generated with NO
   hand patches: regenerate freely with
   `cidx generate github -o .github/workflows/cidx.yml --force`.
3. **probatum** (`~/projects/probatum`) — ✅ WIRED, 0.8.0. `probatum.toml`
   (TOML `[[check]]` tables since 0.8 — it was YAML before) drives the six
   behaviour checks that define the first slice.
4. **configorator** (`~/projects/configurator/configorator`, provisioning +
   deploy from one YAML — adopt last; "one sync to your VPS" is the closing
   act of the demo, not the opening)

**Division of labour — keep it, it prevents duplicate checks:**
cidx owns code quality (clippy, rustfmt, audit, secrets, commit format).
probatum owns behaviour (does the binary DO what it claims). A check belongs
in exactly one of them.

Every friction found in any of the four is an upstream issue to file/fix —
that's half the point. Workflow: draft the issue in chat, get it approved,
then post with `gh issue create`.

**Upstream issues filed from this project — seven, six already closed.**
Every workaround they justified has been deleted; that is the point of tracking
them here rather than letting them calcify into "how stela does things".

Closed and shipped, nothing left in this repo:
- [cidx#190](https://github.com/cidx-org/cidx/issues/190) — `doctor` passed on
  Podman while the Podman executor did not exist. Fixed in v2.1.1.
- [cidx#193](https://github.com/cidx-org/cidx/issues/193) — `go.mod` had no
  `/v2` suffix, so every generated workflow silently installed v1.8.0. The
  bootstrap now emits `.../v3/cmd/cidx@v3.2.0`, pinned. The hand-patched
  clone-and-build bootstrap is gone.
- [cidx#194](https://github.com/cidx-org/cidx/issues/194) — `cargo-audit`
  unpacked into `/usr/local/bin` under a non-root container. The preset now
  unpacks to `/tmp` with `HOME`/`CARGO_HOME` redirected; our override is gone.
- [cidx#195](https://github.com/cidx-org/cidx/issues/195) — a preset producing
  a binary its own runner cannot execute. Fixed for the Go preset and
  documented for the confusing `not found` message.
- [cidx#207](https://github.com/cidx-org/cidx/issues/207) — generated workflows
  had no `permissions` block and no `persist-credentials: false`. Both are
  defaults now, so the workflow is regenerable with no hand patches at all.
- [probatum#1](https://github.com/probatum-org/probatum/issues/1) — HTTP checks
  were GET-only, forcing `run: curl` for any write. Shipped as `post` in 0.2.0.

- [probatum#5](https://github.com/probatum-org/probatum/issues/5) — nothing
  carried from one check to the next, so an authenticated sequence could not be
  expressed. Shipped as a per-run cookie jar in 0.9.0. The division of labour
  never had to bend: the authenticated round trip stayed in `probatum.toml` and
  the Rust integration test was never written.
- [lithair#193](https://github.com/lithair/lithair/issues/193) — no way to set
  an asset's MIME type; extensionless clean URLs defaulted to octet-stream.
  Fixed by `update_asset_with_mime` in lithair-core 1.4.0.
- [lithair#206](https://github.com/lithair/lithair/pull/206) — a PR, not an
  issue: `FrontendServer` answered every miss with a hardcoded page and offered
  no way to replace it. A site that stores `/404.html` now gets it back.
  Merged, shipped in 1.6.0. Together with #193 this deleted `serve_page`
  entirely — 43 lines and the `hyper` direct dependency.

**Preset images are pinned by digest upstream, so they lag by design.** cidx
bumps its `probatum` preset one commit per probatum release; between the two,
override the image in `cidx.toml` (the probatum repo does the same in its own
`.cidx/presets.toml`). Currently overridden to 0.9.0 for the cookie jar.

Closed but the practice stays because it is better anyway:
- [probatum#2](https://github.com/probatum-org/probatum/issues/2) — checks
  against `localhost` failed when it resolved to `::1` and the server bound
  IPv4. `probatum.toml` names `127.0.0.1` explicitly, which beats depending on
  resolver order regardless.

**Not an issue, a self-inflicted one worth remembering:** the musl build
redirects `CARGO_HOME` into the workspace, so ~300 MB of crate sources sit in
`.cargo/`. Trivy read the `Cargo.lock` files that ship *inside* those crates and
reported advisories for versions this project does not use (aws-lc-sys 0.31.0,
while we resolve 0.43.0). The trivy override in cidx.toml skips `.cargo` and
`target`. Check what a scanner is actually reading before believing it.

## Commands

```bash
cargo build            # build
cargo test             # tests
cargo run              # placeholder main for now

cidx run code          # clippy + rustfmt + commitizen
cidx run ci            # full pipeline (security, code, test, build)
cidx validate          # check cidx.toml
cidx doctor            # verify the environment (see the podman caveat above)

probatum run           # behaviour checks; evidence in .probatum/runs/NNNN/

cidx generate github -o .github/workflows/cidx.yml --force   # safe to rerun
```

cidx needs a running Docker daemon; `doctor` now warns rather than passing when
only Podman is present. `probatum` is installed from the local checkout
(`cargo install --path ~/projects/probatum`); it is not on crates.io yet.
Its config is `probatum.toml` — TOML `[[check]]` tables since 0.8, YAML before.
