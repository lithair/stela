//! Stela — single-binary blog engine.
//!
//! Rendering happens on write, not per request: publishing renders the affected
//! pages with Tera and pushes them into Lithair's FrontendEngine, and readers
//! are served finished HTML from memory. See CLAUDE.md for why.

use anyhow::Result;
use bytes::Bytes;
use clap::{Parser, Subcommand};
use http::{Method, Response, StatusCode};
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use lithair_core::app::{DeclarativeModelHandler, LithairServer, ModelHandler};
use lithair_core::frontend::{FrontendEngine, FrontendServer};
use lithair_core::http::{FirewallConfig, HttpExposable, RouteGuard};
use lithair_core::rbac::{RbacUser, ServerRbacConfig};
use lithair_macros::DeclarativeModel;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;

type Resp = Response<BoxBody<Bytes, Infallible>>;

/// Content types for the assets we render. Stated explicitly rather than
/// inferred: a blog's URLs have no extension (`/posts/hello`), and asset MIME
/// detection has nothing else to go on.
const HTML: &str = "text/html; charset=utf-8";
const XML: &str = "application/xml; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";

/// The default theme, compiled in. A theme is a folder of Tera templates + CSS;
/// this one ships inside the binary so `stela serve` needs nothing beside it —
/// that is what "copy one file to your server" requires. Loading a theme from
/// disk to override these comes with the admin, not before.
const THEME: [(&str, &str); 6] = [
    ("index.html", include_str!("../theme/index.html")),
    ("post.html", include_str!("../theme/post.html")),
    ("404.html", include_str!("../theme/404.html")),
    ("admin.html", include_str!("../theme/admin.html")),
    ("login.html", include_str!("../theme/login.html")),
    ("rss.xml", include_str!("../theme/rss.xml")),
];
const STYLE_CSS: &str = include_str!("../theme/style.css");

/// A post. `slug` is the primary key because it is also the URL — a blog post's
/// identity is the address it lives at, and a separate id would only be a second
/// name for the same thing.
#[derive(Debug, Clone, Serialize, Deserialize, DeclarativeModel)]
struct Post {
    #[http(expose)]
    #[db(primary_key)]
    slug: String,

    #[http(expose)]
    title: String,

    /// Markdown source, rendered to HTML at publish time.
    #[http(expose)]
    body: String,

    #[http(expose)]
    published: bool,
}

#[derive(Parser)]
#[command(name = "stela", version, about = "Single-binary blog engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// What `stela new` writes and `stela serve` reads.
///
/// The password is not in here and never will be: only its Argon2id hash is.
/// `stela new` prints the plaintext once and forgets it, so a stolen config
/// gives an attacker a hash to grind rather than a way in.
#[derive(Serialize, Deserialize)]
struct Config {
    admin_route: String,
    admin_user: String,
    admin_password_hash: String,
    title: String,
    description: String,
    base_url: String,
}

const CONFIG_FILE: &str = "stela.toml";

#[derive(Subcommand)]
enum Command {
    /// Create a new blog: a config, a random admin route, and a password.
    New {
        /// Directory to create. Must not already exist.
        path: PathBuf,

        #[arg(long, default_value = "editor")]
        admin_user: String,

        #[arg(long, default_value = "A stela")]
        title: String,
    },

    /// Serve the blog.
    Serve {
        #[arg(short, long, default_value = "3000")]
        port: u16,

        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        #[arg(long, default_value = "./data")]
        data: PathBuf,

        /// Absolute URL of the site, used for links in the feed.
        #[arg(long, default_value = "http://localhost:3000")]
        base_url: String,

        #[arg(long, default_value = "Stela")]
        title: String,

        #[arg(long, default_value = "")]
        description: String,

        /// Where the admin panel answers, e.g. `/secure-a7f3k29`.
        ///
        /// Required, and deliberately without a default: a predictable admin
        /// path is one every scanner already knows. `stela new` generates one
        /// and prints it. This is obscurity, not a lock — the session gate
        /// behind it is what actually protects the panel.
        #[arg(long)]
        admin_route: Option<String>,

        /// Username for the admin panel. The password comes from
        /// STELA_ADMIN_PASSWORD, never from a flag: arguments are visible in
        /// `ps` output and land in shell history.
        #[arg(long)]
        admin_user: Option<String>,
    },
}

/// How long a login lasts before the editor has to sign in again.
const SESSION_HOURS: u64 = 12;

/// Requests per second, per IP, allowed on the login and the admin prefix.
///
/// Generous for a human — nobody types a password ten times a second — and
/// ruinous for a script working through a wordlist.
const LOGIN_QPS: u64 = 10;

/// Everything the templates need that is not a post.
#[derive(Clone, Serialize)]
struct Site {
    title: String,
    description: String,
    base_url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    match Cli::parse().command {
        Command::New {
            path,
            admin_user,
            title,
        } => scaffold(&path, &admin_user, &title),
        Command::Serve {
            port,
            host,
            data,
            base_url,
            title,
            description,
            admin_route,
            admin_user,
        } => {
            serve(
                port,
                host,
                data,
                base_url,
                title,
                description,
                admin_route,
                admin_user,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)] // one per CLI flag; a struct would only move them
async fn serve(
    port: u16,
    host: String,
    data: PathBuf,
    base_url: String,
    title: String,
    description: String,
    admin_route: Option<String>,
    admin_user: Option<String>,
) -> Result<()> {
    // stela.toml if there is one, flags otherwise. `stela new` writes the file
    // so a person never has to hold a route and a password in their head just
    // to start their own blog.
    let config = load_config()?;

    let admin_route = admin_route
        .or_else(|| config.as_ref().map(|c| c.admin_route.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no admin route: pass --admin-route, or run `stela new <dir>` and serve \
                 from that directory. There is deliberately no default — a predictable \
                 admin path is one every scanner already knows."
            )
        })?;
    if !admin_route.starts_with('/') {
        anyhow::bail!("--admin-route must start with '/' (got {admin_route:?})");
    }
    let admin_user = admin_user
        .or_else(|| config.as_ref().map(|c| c.admin_user.clone()))
        .unwrap_or_else(|| "editor".to_string());

    // The hash from the config if there is one — that is the path where this
    // binary never sees the plaintext at all. Otherwise the environment, hashed
    // here and dropped three lines later. Refused rather than defaulted either
    // way: a blog with an admin panel and no password is not a degraded blog,
    // it is a defaced one waiting to happen.
    let admin_password_hash = match config.as_ref() {
        Some(c) => c.admin_password_hash.clone(),
        None => {
            let admin_password = std::env::var("STELA_ADMIN_PASSWORD").map_err(|_| {
                anyhow::anyhow!(
                    "STELA_ADMIN_PASSWORD is not set and there is no {CONFIG_FILE} here. \
                     The admin panel needs a password, and it is read from the environment \
                     rather than a flag so it stays out of `ps` output and shell history."
                )
            })?;
            let hash = lithair_core::security::hash_password(&admin_password)
                .map_err(|e| anyhow::anyhow!("could not hash the admin password: {e}"))?;
            drop(admin_password);
            hash
        }
    };

    let site = Site {
        title: config.as_ref().map(|c| c.title.clone()).unwrap_or(title),
        description: config
            .as_ref()
            .map(|c| c.description.clone())
            .unwrap_or(description),
        base_url: config
            .as_ref()
            .map(|c| c.base_url.clone())
            .unwrap_or(base_url),
    };
    let posts_dir = data.join("posts").to_string_lossy().to_string();

    let engine = Arc::new(FrontendEngine::new("stela", data.join("frontend")).await?);

    // The stylesheet never changes between rebuilds, so it is pushed once.
    engine
        .update_asset_with_mime("/style.css", STYLE_CSS.as_bytes().to_vec(), CSS)
        .await?;

    // Render before serving: `/` has to answer 200 from the first request, even
    // with no posts, or a readiness probe never passes.
    rebuild(&engine, &posts_dir, &site).await?;

    log::info!("stela serving on http://{host}:{port}");
    log::info!("admin panel: http://{host}:{port}{admin_route} (user: {admin_user})");
    log::warn!(
        "that admin URL is defence in depth, not a lock — it leaks through Referer \
         headers, browser history and proxy logs. What protects the panel is the \
         session behind it. Serve this over HTTPS."
    );

    // A write signals a rebuild instead of the caller having to ask for one.
    // Capacity 1 and try_send: if a rebuild is already pending, dropping the
    // signal is correct rather than lossy — rebuild() replays the whole store,
    // so the pending one will render whatever the newer write left behind. That
    // is what keeps a bulk import from queueing one full rebuild per post.
    let (dirty_tx, mut dirty_rx) = tokio::sync::mpsc::channel::<()>(1);
    let hook_engine = engine.clone();
    let hook_dir = posts_dir.clone();
    let hook_site = site.clone();
    tokio::spawn(async move {
        while dirty_rx.recv().await.is_some() {
            if let Err(e) = rebuild(&hook_engine, &hook_dir, &hook_site).await {
                log::error!("a rebuild triggered by a write failed: {e}");
            }
        }
    });

    let rebuild_engine = engine.clone();
    let rebuild_dir = posts_dir.clone();
    let rebuild_site = site.clone();
    let frontend_server = Arc::new(FrontendServer::new_scc2(engine.clone()));
    let login_site = site.clone();
    let login_route = admin_route.clone();
    let admin_site = site.clone();
    let admin_dir = posts_dir.clone();
    let admin_route_for_page = admin_route.clone();

    LithairServer::new()
        .with_port(port)
        .with_host(&host)
        // Auth is Lithair's, not a local reimplementation: it mounts login,
        // logout and validate under the prefix, hashes with Argon2id, and
        // issues the session cookie a browser needs (1.9, lithair#220).
        // with_auth_path must come first — with_rbac_config registers those
        // routes as it runs.
        .with_auth_path(&admin_route)
        .with_rbac_config(ServerRbacConfig {
            roles: vec![("Editor".to_string(), vec!["*".to_string()])],
            users: vec![RbacUser::new_with_hashed_password(
                &admin_user,
                &admin_password_hash,
                "Editor",
            )],
            session_store_path: Some(data.join("sessions").to_string_lossy().to_string()),
            session_duration: SESSION_HOURS * 3600,
        })
        // Gates every auto-generated /api/* route: no live session, no write.
        // Reads of the blog itself do not go through here — they are assets.
        .with_models_require_session(true)
        // One guarded prefix instead of a list of guarded paths. Everything
        // administrative — the editor, the rebuild, Lithair's dashboard, the
        // data admin — hangs off the operator's own route, so there is nothing
        // at a guessable path to authenticate against in the first place. That
        // is the whole difference from a /wp-admin.
        .with_route_guard(
            admin_route.clone(),
            RouteGuard::RequireAuth {
                redirect_to: None,
                exclude: vec![],
            },
        )
        .with_route_guard(
            format!("{admin_route}/*"),
            RouteGuard::RequireAuth {
                redirect_to: None,
                // The login page and the login itself are the only things under
                // the prefix that must answer without a session — they are how
                // a session is obtained. Everything else stays shut.
                exclude: vec![format!("{admin_route}/login")],
            },
        )
        // Lithair ships a dashboard and a data admin. Building a blog on this
        // framework and then reimplementing them would be the wrong showcase —
        // they are mounted under the same secret prefix instead, so they
        // inherit the same "nothing to find" property.
        .with_admin_panel(true)
        .with_admin_auth(true)
        .with_admin_path(format!("{admin_route}/panel"))
        .with_data_admin_ui(format!("{admin_route}/data"))
        // The login is the only endpoint in the binary that answers without a
        // session, and it now sits behind the secret prefix like everything
        // else — so this limit applies to someone who already knows the prefix.
        // That is what makes it defence in depth rather than the front door.
        // Public reads stay outside it: a blog that throttles readers has
        // misunderstood what it is for.
        .with_firewall_config(FirewallConfig {
            enabled: true,
            allow: Default::default(),
            deny: Default::default(),
            global_qps: None,
            per_ip_qps: Some(LOGIN_QPS),
            protected_prefixes: vec![format!("{admin_route}/login")],
            exempt_prefixes: vec![],
        })
        .with_route(Method::GET, format!("{admin_route}/login"), move |_req| {
            let site = login_site.clone();
            let route = login_route.clone();
            Box::pin(async move { Ok(html(StatusCode::OK, login_html(&site, &route))) })
        })
        .with_route(Method::GET, admin_route.clone(), move |_req| {
            let site = admin_site.clone();
            let dir = admin_dir.clone();
            let route = admin_route_for_page.clone();
            Box::pin(async move {
                let posts = load_posts(&dir).await.unwrap_or_else(|e| {
                    log::error!("the editor could not read the post store: {e}");
                    Vec::new()
                });
                Ok(html(StatusCode::OK, admin_html(&site, &route, &posts)))
            })
        })
        .with_model::<Post>(posts_dir.clone(), "/api/posts")
        // lithair 1.8 (issue #70). Without it a post written through the REST
        // API is stored and invisible until somebody remembers to call rebuild
        // — which a headless client has no reason to know about. The hook runs
        // after the write has reached the event store, which is what makes the
        // replay inside rebuild() see it.
        //
        // The hook must stay short and is not async, so it only signals; the
        // worker above does the work.
        .on_mutation(Post::http_base_path(), move |event| {
            log::info!("{} on {}, rebuilding", event.operation, event.model_name);
            let _ = dirty_tx.try_send(());
        })
        .with_route(
            Method::POST,
            format!("{admin_route}/rebuild"),
            move |_req| {
                let engine = rebuild_engine.clone();
                let dir = rebuild_dir.clone();
                let site = rebuild_site.clone();
                Box::pin(async move {
                    match rebuild(&engine, &dir, &site).await {
                        Ok(n) => Ok(json(StatusCode::OK, &format!(r#"{{"rendered":{n}}}"#))),
                        Err(e) => {
                            log::error!("rebuild failed: {e}");
                            Ok(json(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                r#"{"error":"rebuild failed"}"#,
                            ))
                        }
                    }
                })
            },
        )
        // Lithair serves every rendered page straight from SCC2 memory. Stela
        // hand-rolled this until lithair#206: the MIME type now travels with
        // the asset, and a miss returns the theme's /404.html rather than the
        // framework's built-in page, so there is nothing left to reimplement.
        .with_route(Method::GET, "/*".to_string(), move |req| {
            let server = frontend_server.clone();
            Box::pin(async move {
                Ok(server
                    .handle_request(req)
                    .await
                    .unwrap_or_else(|e| match e {}))
            })
        })
        .serve()
        .await?;

    Ok(())
}

/// Render every published post plus the index and the feed, and push the result
/// into memory.
///
/// The handler is built here and dropped at the end on purpose: `new()` replays
/// the event log from disk, so this sees writes made through the REST API even
/// though that API owns its own handler inside the server. It is also why no
/// Lithair change is needed to trigger a rebuild — see CLAUDE.md.
async fn rebuild(engine: &FrontendEngine, posts_dir: &str, site: &Site) -> Result<usize> {
    let stored = load_posts(posts_dir).await?;

    let now = chrono::Utc::now().to_rfc2822();
    let mut posts: Vec<serde_json::Value> = stored
        .into_iter()
        .filter(|p| p.published)
        .map(|p| {
            serde_json::json!({
                "slug": p.slug,
                "title": p.title,
                // Built here rather than concatenated in the template so the
                // feed can mark it `safe`: Tera's escape_xml turns every "/"
                // into "&#x2F;", which is valid XML but not a URL anyone
                // wants to read. Safe to trust only because slug_is_safe ran.
                "url": format!("{}/posts/{}", site.base_url.trim_end_matches('/'), p.slug),
                "body_html": markdown_to_html(&p.body),
                // ponytail: no per-post date field yet, so every item carries
                // the build time. Add `published_at` to Post when someone
                // complains about feed ordering — the first symptom that
                // actually matters.
                "published_rfc2822": now,
            })
        })
        .collect();

    posts.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    let tera = theme()?;
    let mut ctx = tera::Context::new();
    ctx.insert("site", site);
    ctx.insert("posts", &posts);

    engine
        .update_asset_with_mime(
            "/index.html",
            tera.render("index.html", &ctx)?.into_bytes(),
            HTML,
        )
        .await?;
    engine
        .update_asset_with_mime(
            "/404.html",
            tera.render("404.html", &ctx)?.into_bytes(),
            HTML,
        )
        .await?;
    engine
        .update_asset_with_mime("/rss.xml", tera.render("rss.xml", &ctx)?.into_bytes(), XML)
        .await?;

    for post in &posts {
        let mut page = ctx.clone();
        page.insert("post", post);
        let slug = post["slug"].as_str().unwrap_or_default();
        let html = tera.render("post.html", &page)?;
        engine
            .update_asset_with_mime(&format!("/posts/{slug}"), html.into_bytes(), HTML)
            .await?;
    }

    log::info!("rebuilt {} published post(s)", posts.len());
    Ok(posts.len())
}

/// Read `stela.toml` from the working directory, if there is one.
fn load_config() -> Result<Option<Config>> {
    let path = std::path::Path::new(CONFIG_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("could not read {CONFIG_FILE}: {e}"))?;
    let config: Config =
        toml::from_str(&text).map_err(|e| anyhow::anyhow!("{CONFIG_FILE} is not valid: {e}"))?;
    Ok(Some(config))
}

/// Create a new blog: a directory, a config, a random admin route and a
/// random password.
///
/// The password is printed once and never stored — only its Argon2id hash goes
/// into the config. That is the whole reason this command exists rather than
/// leaving people to invent a route and export a variable: both of the values
/// that matter are generated unguessable, and the secret one is never written
/// down by the machine.
fn scaffold(path: &std::path::Path, admin_user: &str, title: &str) -> Result<()> {
    if path.exists() {
        anyhow::bail!(
            "{} already exists. Refusing to write into it — generating a config over \
             an existing blog would replace its admin route and password, locking the \
             owner out of their own site.",
            path.display()
        );
    }

    let admin_route = format!("/secure-{}", random_token(6));
    let password = random_token(12);
    let admin_password_hash = lithair_core::security::hash_password(&password)
        .map_err(|e| anyhow::anyhow!("could not hash the generated password: {e}"))?;

    let config = Config {
        admin_route: admin_route.clone(),
        admin_user: admin_user.to_string(),
        admin_password_hash,
        title: title.to_string(),
        description: String::new(),
        base_url: "http://localhost:3000".to_string(),
    };

    std::fs::create_dir_all(path)
        .map_err(|e| anyhow::anyhow!("could not create {}: {e}", path.display()))?;
    let body = format!(
        "# Written by `stela new`. The password is NOT here — only its Argon2id\n\
         # hash. If you lose it, generate a new blog or replace the hash by hand.\n\
         {}",
        toml::to_string_pretty(&config)?
    );
    std::fs::write(path.join(CONFIG_FILE), body)
        .map_err(|e| anyhow::anyhow!("could not write the config: {e}"))?;

    // Deliberately not logged: log output goes to files, gets shipped to
    // aggregators and outlives the terminal. This is printed, once.
    println!("Created {}", path.display());
    println!();
    println!("Write these down now — the password is not stored anywhere:");
    println!();
    println!("Admin route: {admin_route}");
    println!("Password: {password}");
    println!("Username: {admin_user}");
    println!();
    println!("  cd {} && stela serve", path.display());
    println!();
    println!("The admin route is not a lock, it only keeps scanners from finding");
    println!("the panel. Serve this over HTTPS.");

    Ok(())
}

/// A random lowercase-alphanumeric token, from the OS random source.
///
/// Used for the admin route and the first password. Both are things a stranger
/// must not be able to guess, so this is `getrandom` rather than a timestamp or
/// a counter. Rejection sampling keeps every character equally likely — a plain
/// modulo would quietly bias the alphabet and shrink the search space.
fn random_token(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 32];
    while out.len() < len {
        getrandom::fill(&mut buf).expect("the OS random source must be available");
        for b in buf {
            if out.len() == len {
                break;
            }
            // 256 is a multiple of 32, so masking is already unbiased here; the
            // mask is kept explicit so a future alphabet change is obviously
            // wrong rather than subtly skewed.
            out.push(ALPHABET[(b & 0x1f) as usize] as char);
        }
    }
    out
}

/// Every post in the store, unusable slugs already dropped.
///
/// The handler is built here and dropped at the end on purpose: `new()` replays
/// the event log from disk, so this sees writes made through the REST API even
/// though that API owns its own handler inside the server. It is also why no
/// Lithair change is needed to trigger a rebuild — see CLAUDE.md.
///
/// ponytail: a replay per call, which the editor pays on every page view. Fine
/// for a blog; if a site ever grows enough posts for it to show, cache it and
/// invalidate on rebuild.
async fn load_posts(posts_dir: &str) -> Result<Vec<Post>> {
    let handler = DeclarativeModelHandler::<Post>::new(posts_dir.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("could not open the post store: {e}"))?;

    Ok(
        serde_json::from_value::<Vec<Post>>(handler.get_all_data_json().await)
            .unwrap_or_default()
            .into_iter()
            // The slug arrives straight from the REST API and becomes an asset path.
            // An unchecked one writes wherever it likes (`../../x`) and lands
            // unescaped in the feed's URLs. Filtered here, where every caller passes.
            .filter(|p| {
                let ok = slug_is_safe(&p.slug);
                if !ok {
                    log::warn!("skipping post with unusable slug: {:?}", p.slug);
                }
                ok
            })
            .collect(),
    )
}

/// A slug has to be safe as a URL path segment and as an asset key.
///
/// Deliberately strict rather than clever: lowercase ASCII, digits, `-` and `_`.
/// Anything else — a slash, a dot, a percent escape, a non-ASCII letter — is
/// rejected instead of transliterated, because silently renaming someone's post
/// URL is worse than telling them the slug is unusable.
fn slug_is_safe(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 200
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn theme() -> Result<tera::Tera> {
    let mut tera = tera::Tera::default();
    tera.add_raw_templates(THEME.to_vec())?;
    Ok(tera)
}

fn markdown_to_html(markdown: &str) -> String {
    let parser = pulldown_cmark::Parser::new(markdown);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    html
}

/// The editor page.
///
/// Rendered per request rather than pushed into the asset store, because the
/// asset store is what `FrontendServer` hands to the public — an admin page
/// living there would be one routing mistake away from being world-readable.
/// It also has to show drafts, which by definition are not in the rendered site.
fn admin_html(site: &Site, admin_route: &str, posts: &[Post]) -> String {
    let mut ctx = tera::Context::new();
    ctx.insert("site", site);
    ctx.insert("admin_route", admin_route);
    ctx.insert("posts", posts);
    // Serialised once for the page's script so clicking a post fills the form
    // from what is already loaded instead of fetching it again.
    ctx.insert("posts_json", &posts_as_script_json(posts));
    theme()
        .and_then(|t| t.render("admin.html", &ctx).map_err(Into::into))
        .unwrap_or_else(|e| {
            log::error!("the bundled admin template failed to render: {e}");
            "<h1>Editor unavailable</h1>".to_string()
        })
}

/// Serialise posts for embedding inside a `<script>` block.
///
/// `</` is escaped to `<\/`. Inside a script element the HTML parser stops at
/// the first `</script`, wherever it appears — including inside a JavaScript
/// string — so a post whose body contains that sequence would close the block
/// and let the rest of the body run as markup. The escape is invalid in strict
/// JSON but valid in a JavaScript string literal, which is what this becomes,
/// and it parses back to the same characters.
fn posts_as_script_json(posts: &[Post]) -> String {
    serde_json::to_string(posts)
        .unwrap_or_else(|_| "[]".to_string())
        .replace("</", "<\\/")
}

/// The sign-in page. The only page in the binary served without a session, and
/// only to someone who already knows the prefix.
fn login_html(site: &Site, admin_route: &str) -> String {
    let mut ctx = tera::Context::new();
    ctx.insert("site", site);
    ctx.insert("admin_route", admin_route);
    theme()
        .and_then(|t| t.render("login.html", &ctx).map_err(Into::into))
        .unwrap_or_else(|e| {
            log::error!("the bundled login template failed to render: {e}");
            "<h1>Sign in unavailable</h1>".to_string()
        })
}

fn html(status: StatusCode, body: String) -> Resp {
    Response::builder()
        .status(status)
        .header("Content-Type", HTML)
        .body(Full::new(Bytes::from(body)).boxed())
        .expect("response builder cannot fail on a static header set")
}

fn json(status: StatusCode, body: &str) -> Resp {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())).boxed())
        .expect("response builder cannot fail on a static header set")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_html() {
        assert_eq!(markdown_to_html("# Salut").trim(), "<h1>Salut</h1>");
        assert!(markdown_to_html("a **bold** word").contains("<strong>bold</strong>"));
    }

    #[test]
    fn unusable_slugs_are_refused() {
        assert!(slug_is_safe("hello"));
        assert!(slug_is_safe("a-post_2"));

        // Path traversal — this one would write an asset outside /posts/.
        assert!(!slug_is_safe("../../etc/passwd"));
        assert!(!slug_is_safe("a/b"));
        // Would land unescaped in the feed or the page.
        assert!(!slug_is_safe("a<b"));
        assert!(!slug_is_safe("a&b"));
        assert!(!slug_is_safe("a b"));
        // Ambiguous or useless.
        assert!(!slug_is_safe(""));
        assert!(!slug_is_safe("Hello"));
        assert!(!slug_is_safe(&"x".repeat(201)));
    }

    #[test]
    fn a_post_cannot_break_out_of_the_editor_script_block() {
        let posts = vec![Post {
            slug: "x".into(),
            title: "t".into(),
            body: "</script><img src=x onerror=alert(1)>".into(),
            published: true,
        }];

        let json = posts_as_script_json(&posts);

        assert!(
            !json.contains("</script"),
            "the script block can be closed: {json}"
        );
        assert!(json.contains("<\\/script"));
    }

    #[test]
    fn theme_templates_compile() {
        let tera = theme().expect("bundled theme must always parse");
        let mut names: Vec<_> = tera.get_template_names().collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "404.html",
                "admin.html",
                "index.html",
                "login.html",
                "post.html",
                "rss.xml"
            ]
        );
    }
}
