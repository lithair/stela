# Stela

**Single-binary blog engine. Your words, set in stone.**

> ⚠️ Works, but unreleased. The engine below does what it says — you can
> scaffold a blog, sign in, write and publish. What is not done is shipping it:
> 0.0.1 on crates.io is still a name reservation, and no image is published
> yet, so today it is `cargo build` or `docker build`.

A *stela* is a standing stone bearing a public inscription — laws, poems,
announcements. The blog of antiquity. Stela is the modern one: a blog engine
that deploys in two commands and runs as **one binary**.

```
stela new myblog
stela serve
```

## What it will be

- **One binary, zero infrastructure.** No external database, no Node at
  runtime, no external build tool. One statically linked file that runs on any
  Linux — copy it to your server and start it. Your content lives in an
  event-sourced store on disk, served memory-first by
  [Lithair](https://github.com/lithair/lithair).
- **Publish instantly.** Hitting publish renders the affected pages inside the
  binary with [Tera](https://keats.github.io/tera/) templates and puts them
  straight into memory — no external generator, no rebuild pipeline, no
  restart. Readers are served finished HTML, so a page view never waits on a
  template engine.
- **Themes are folders.** A theme is Tera templates + CSS. If you know Zola's
  template syntax, you already know how to write one. Site settings (title,
  colors, menus) are editable from the admin and apply as soon as you save.
- **Secure by default.** Public reads, RBAC-protected writes, sessions,
  firewall and rate limiting — inherited from Lithair, not bolted on.
- **Headless when you want it.** The content API is plain REST; point Astro
  or anything else at it if a static front is your thing.

## What works today

```bash
stela new myblog     # writes a config with a random admin route,
                     # prints the route and a generated password once
cd myblog && stela serve
```

Public reads, session-gated writes, a markdown editor, drafts, and an RSS feed.
Every administrative surface — the editor, the rebuild, Lithair's dashboard,
the login — hangs off one per-install random prefix, so a scanner walking the
usual `/wp-admin` list finds nothing to authenticate against.

There is also a `Dockerfile` that ships `FROM scratch`: the image holds the
static binary and nothing else, so it has no shell and no distribution to track
CVEs for.

## Status

Unreleased. Not deployed anywhere, not published to a registry, no tagged
version. Follow progress at
[github.com/lithair/stela](https://github.com/lithair/stela).

## License

MIT OR Apache-2.0
