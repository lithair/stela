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
use lithair_core::http::{utils::Req, RouteGuard};
use lithair_core::session::{PersistentSessionStore, Session, SessionManager, SessionStore};
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
const THEME: [(&str, &str); 5] = [
    ("index.html", include_str!("../theme/index.html")),
    ("post.html", include_str!("../theme/post.html")),
    ("404.html", include_str!("../theme/404.html")),
    ("admin.html", include_str!("../theme/admin.html")),
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

#[derive(Subcommand)]
enum Command {
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
        admin_route: String,

        /// Username for the admin panel. The password comes from
        /// STELA_ADMIN_PASSWORD, never from a flag: arguments are visible in
        /// `ps` output and land in shell history.
        #[arg(long)]
        admin_user: String,
    },
}

/// Name of the session cookie. Not arbitrary: Lithair's session extractor
/// (`http/declarative.rs`) looks for exactly this, and it is what makes the
/// model gate and the route guards accept a browser session.
const SESSION_COOKIE: &str = "session_token";

/// How long a login lasts before the editor has to sign in again.
const SESSION_HOURS: i64 = 12;

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

    let Cli { command } = Cli::parse();
    let Command::Serve {
        port,
        host,
        data,
        base_url,
        title,
        description,
        admin_route,
        admin_user,
    } = command;

    // Refused rather than defaulted. A blog with an admin panel and no password
    // is not a degraded blog, it is a defaced one waiting to happen, so the
    // binary does not start without a way to lock it.
    let admin_password = std::env::var("STELA_ADMIN_PASSWORD").map_err(|_| {
        anyhow::anyhow!(
            "STELA_ADMIN_PASSWORD is not set. The admin panel needs a password, and it \
             is read from the environment rather than a flag so it stays out of `ps` \
             output and shell history."
        )
    })?;
    if !admin_route.starts_with('/') {
        anyhow::bail!("--admin-route must start with '/' (got {admin_route:?})");
    }
    // Hashed once at boot so the comparison on every login attempt is Argon2's,
    // which is constant-time, instead of a byte-by-byte match on the plaintext.
    let admin_password_hash = lithair_core::security::hash_password(&admin_password)
        .map_err(|e| anyhow::anyhow!("could not hash the admin password: {e}"))?;
    drop(admin_password);

    let site = Site {
        title,
        description,
        base_url,
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

    let sessions = Arc::new(PersistentSessionStore::new(data.join("sessions"))?);

    log::info!("stela serving on http://{host}:{port}");
    log::info!("admin panel: http://{host}:{port}{admin_route} (user: {admin_user})");
    log::warn!(
        "that admin URL is defence in depth, not a lock — it leaks through Referer \
         headers, browser history and proxy logs. What protects the panel is the \
         session behind it. Serve this over HTTPS."
    );

    let rebuild_engine = engine.clone();
    let rebuild_dir = posts_dir.clone();
    let rebuild_site = site.clone();
    let frontend_server = Arc::new(FrontendServer::new_scc2(engine.clone()));
    let login_sessions = sessions.clone();
    let logout_sessions = sessions.clone();
    let login_user = admin_user.clone();
    let admin_page = admin_html(&site, &admin_route);

    LithairServer::new()
        .with_port(port)
        .with_host(&host)
        .with_sessions(SessionManager::from_arc(sessions.clone()))
        // Gates every auto-generated /api/* route: no live session, no write.
        // Reads of the blog itself do not go through here — they are assets.
        .with_models_require_session(true)
        .with_route_guard(
            "/admin/*",
            RouteGuard::RequireAuth {
                redirect_to: None,
                exclude: vec![],
            },
        )
        .with_route_guard(
            admin_route.clone(),
            RouteGuard::RequireAuth {
                redirect_to: None,
                exclude: vec![],
            },
        )
        .with_route(Method::POST, "/auth/login".to_string(), move |req| {
            let store = login_sessions.clone();
            let user = login_user.clone();
            let hash = admin_password_hash.clone();
            Box::pin(async move { Ok(login(req, store, &user, &hash).await) })
        })
        .with_route(Method::POST, "/auth/logout".to_string(), move |req| {
            let store = logout_sessions.clone();
            Box::pin(async move { Ok(logout(req, store).await) })
        })
        .with_route(Method::GET, admin_route.clone(), move |_req| {
            let page = admin_page.clone();
            Box::pin(async move { Ok(html(StatusCode::OK, page)) })
        })
        .with_model::<Post>(posts_dir.clone(), "/api/posts")
        .with_route(Method::POST, "/admin/rebuild".to_string(), move |_req| {
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
        })
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
    let handler = DeclarativeModelHandler::<Post>::new(posts_dir.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("could not open the post store: {e}"))?;

    let now = chrono::Utc::now().to_rfc2822();
    let mut posts: Vec<serde_json::Value> =
        serde_json::from_value::<Vec<Post>>(handler.get_all_data_json().await)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.published)
            .filter(|p| {
                // The slug reaches us straight from the REST API and is used to
                // build an asset path. An unchecked one writes wherever it likes
                // (`../../x`) and lands unescaped in the feed's URLs. Refuse it
                // here, where every rendered page funnels through.
                let ok = slug_is_safe(&p.slug);
                if !ok {
                    log::warn!("skipping post with unusable slug: {:?}", p.slug);
                }
                ok
            })
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

/// Open a session for the admin, or refuse.
///
/// Both failure modes answer the same 401 with the same body: telling an
/// attacker whether the username exists narrows their search for free.
async fn login(
    req: Req,
    sessions: Arc<PersistentSessionStore>,
    admin_user: &str,
    admin_password_hash: &str,
) -> Resp {
    #[derive(Deserialize)]
    struct Credentials {
        username: String,
        password: String,
    }

    let refused = || {
        json(
            StatusCode::UNAUTHORIZED,
            r#"{"error":"invalid credentials"}"#,
        )
    };

    let Ok(body) = req.into_body().collect().await else {
        return refused();
    };
    let Ok(creds) = serde_json::from_slice::<Credentials>(&body.to_bytes()) else {
        return refused();
    };

    // Argon2 verification runs even when the username is wrong, so a bad
    // username and a bad password take the same time to refuse.
    let password_ok = lithair_core::security::verify_password(&creds.password, admin_password_hash)
        .unwrap_or(false);
    if creds.username != admin_user || !password_ok {
        log::warn!("refused a login for {:?}", creds.username);
        return refused();
    }

    let token = new_session_token();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(SESSION_HOURS);
    let mut session = Session::new(token.clone(), expires_at);
    if session.set("user", &creds.username).is_err() || sessions.set(session).await.is_err() {
        log::error!("could not persist a session for {:?}", creds.username);
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error":"session store failed"}"#,
        );
    }

    log::info!("{} logged in", creds.username);
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        // HttpOnly keeps the token away from page scripts, SameSite=Strict stops
        // another site driving the admin API with the editor's session, and
        // Secure means it never travels in clear — which does assume HTTPS, as
        // the startup warning says.
        .header(
            "Set-Cookie",
            format!(
                "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age={}",
                SESSION_HOURS * 3600
            ),
        )
        .body(Full::new(Bytes::from(r#"{"status":"ok"}"#)).boxed())
        .expect("response builder cannot fail on a static header set")
}

/// Close the session and clear the cookie.
///
/// Answers 200 whether or not a session was found: "you are logged out" is true
/// either way, and reporting the difference only tells a caller whether the
/// token it presented was live.
async fn logout(req: Req, sessions: Arc<PersistentSessionStore>) -> Resp {
    if let Some(token) = session_cookie(&req) {
        if sessions.delete(&token).await.is_err() {
            log::warn!("could not delete a session on logout");
        }
    }

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header(
            "Set-Cookie",
            format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age=0"),
        )
        .body(Full::new(Bytes::from(r#"{"status":"ok"}"#)).boxed())
        .expect("response builder cannot fail on a static header set")
}

fn session_cookie(req: &Req) -> Option<String> {
    req.headers()
        .get(http::header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| part.trim().strip_prefix(&format!("{SESSION_COOKIE}=")))
        .map(str::to_string)
        .filter(|t| !t.is_empty())
}

/// A session token, 256 bits from the OS random source.
///
/// `getrandom` rather than a counter or a timestamp: this value is the only
/// thing standing between a stranger and the admin panel, so it has to be
/// unguessable, not merely unique.
fn new_session_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS random source must be available");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The admin landing page, rendered once at startup.
///
/// It never changes between requests, so it is not worth re-rendering on each
/// one — and unlike the blog's pages it is not pushed into the asset store,
/// because assets are what `FrontendServer` serves to the public.
fn admin_html(site: &Site, admin_route: &str) -> String {
    let mut ctx = tera::Context::new();
    ctx.insert("site", site);
    ctx.insert("admin_route", admin_route);
    theme()
        .and_then(|t| t.render("admin.html", &ctx).map_err(Into::into))
        .unwrap_or_else(|e| {
            log::error!("the bundled admin template failed to render: {e}");
            "<h1>Admin</h1>".to_string()
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
                "post.html",
                "rss.xml"
            ]
        );
    }
}
