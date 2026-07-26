# Stela

**Single-binary blog engine. Your words, set in stone.**

> ⚠️ In development — this 0.0.1 release reserves the crate name and announces
> the project. Nothing usable ships yet.

A *stela* is a standing stone bearing a public inscription — laws, poems,
announcements. The blog of antiquity. Stela is the modern one: a blog engine
that deploys in two commands and runs as **one binary**.

```
stela new myblog
stela serve
```

## What it will be

- **One binary, zero infrastructure.** No external database, no Node at
  runtime, no build pipeline. Your content lives in an event-sourced store on
  disk, served memory-first by [Lithair](https://github.com/lithair/lithair).
- **Publish instantly.** Pages are rendered server-side with
  [Tera](https://keats.github.io/tera/) templates — no static-site rebuild
  step. Hit publish, it's live.
- **Themes are folders.** A theme is Tera templates + CSS. If you know Zola's
  template syntax, you already know how to write one. Site settings (title,
  colors, menus) are editable from the admin and take effect immediately.
- **Secure by default.** Public reads, RBAC-protected writes, sessions,
  firewall and rate limiting — inherited from Lithair, not bolted on.
- **Headless when you want it.** The content API is plain REST; point Astro
  or anything else at it if a static front is your thing.

## Status

Design phase. Follow progress at
[github.com/lithair/stela](https://github.com/lithair/stela).

## License

MIT OR Apache-2.0
