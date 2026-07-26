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

- **Rendering: server-side Tera templates inside the binary.** Chosen over an
  Astro/Node front (Node dependency, rebuild-on-publish complexity) and over
  shelling out to Zola (two sources of truth). Publishing is instant: no
  static-site build step exists. Zola cannot be embedded (it's a binary, not a
  library); Tera is its template engine and IS a crate.
- **Theme = a folder of Tera templates + CSS**, loaded at startup. Pitch
  "Tera syntax, porting a Zola theme is reasonable work" — do NOT promise
  "Zola-theme compatible" (its built-ins `get_section`, shortcodes, taxonomies
  would each need reimplementing; add the top 2-3 only when a real port asks).
- **Content models: `Post`, `Page`, `SiteSettings`** via
  `#[derive(DeclarativeModel)]`. Public read / RBAC write is the canonical
  Lithair pattern (see lithair's rbac-session example). `SiteSettings` feeds
  the Tera context, so admin edits (title, colors, menus) apply immediately.
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

1. **lithair** (runtime — day 1, it IS the product)
2. **cidx** (CI — day 1, copy the proven cidx.toml pattern from the lithair repo)
3. **probatum** (`~/projects/probatum`, declarative test runner with embedded
   curl/grep/process checks — adopt for E2E smoke as soon as the binary
   serves something: start, hit `/`, validate RSS)
4. **configorator** (`~/projects/configurator/configorator`, provisioning +
   deploy from one YAML — adopt last; "one sync to your VPS" is the closing
   act of the demo, not the opening)

Every friction found in any of the four is an upstream issue to file/fix —
that's half the point.

## Commands

```bash
cargo build            # build
cargo test             # tests
cargo run              # placeholder main for now
```
