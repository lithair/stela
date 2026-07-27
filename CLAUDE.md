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
  UNVERIFIED: that the event store flushes synchronously on write — if it
  buffers, the replay misses just-written posts. Check at implementation.
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
- **Headless stays free:** the REST content API comes with DeclarativeModel;
  an Astro/other front consuming it is an advanced path, never the default.
- **Storage:** Lithair event store on disk (`data/`), memory-first serving.
  No external DB, ever — that's the product's reason to exist.

## MVP scope (and non-goals)

MVP: `Post` (slug, title, markdown body, published flag), `Page`,
`SiteSettings`, admin UI behind session/RBAC, one default theme (index,
article, page, RSS + one CSS file), markdown via `pulldown-cmark`.

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

1. **lithair** (runtime — day 1, it IS the product) — NOT WIRED YET
2. **cidx** (CI) — ✅ WIRED. `cidx.toml` generated by `cidx init`, minus the
   auto-added `prettier` (Rust-only repo; its sole target was the
   hand-wrapped Markdown). Phase `code` is green.
3. **probatum** (`~/projects/probatum`) — ✅ WIRED. `probatum.yaml` is thin on
   purpose: today the binary only prints a banner. It grows into the real
   E2E smoke the moment `stela serve` exists — start, GET `/`, validate RSS.
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

**Upstream issues filed from this project:**
- [cidx#190](https://github.com/cidx-org/cidx/issues/190) — `cidx doctor`
  passes on Podman (`pkg/doctor/doctor.go:87`) while the Podman executor does
  not exist (`pkg/executor/selector.go:81`). Green light on an environment
  where no pipeline can run. Open, awaiting a direction before any PR.

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
```

cidx needs a running Docker daemon — podman is detected but unsupported.
`probatum` is installed from the local checkout
(`cargo install --path ~/projects/probatum`); it is not on crates.io yet.
