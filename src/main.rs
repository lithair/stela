//! Stela — single-binary blog engine.
//!
//! Rendering happens on write, not per request: publishing renders the affected
//! pages with Tera and pushes them into Lithair's FrontendEngine, and readers
//! are served finished HTML from memory. See CLAUDE.md for why.

use anyhow::Result;
use bytes::Bytes;
use clap::{Parser, Subcommand};
use http::{Method, Request, Response, StatusCode};
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use lithair_core::app::{DeclarativeModelHandler, LithairServer, ModelHandler};
use lithair_core::frontend::FrontendEngine;
use lithair_macros::DeclarativeModel;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;

type Resp = Response<BoxBody<Bytes, Infallible>>;

/// The default theme, compiled in. A theme is a folder of Tera templates + CSS;
/// this one ships inside the binary so `stela serve` needs nothing beside it —
/// that is what "copy one file to your server" requires. Loading a theme from
/// disk to override these comes with the admin, not before.
const THEME: [(&str, &str); 3] = [
    ("index.html", include_str!("../theme/index.html")),
    ("post.html", include_str!("../theme/post.html")),
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
    },
}

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
    } = command;

    let site = Site {
        title,
        description,
        base_url,
    };
    let posts_dir = data.join("posts").to_string_lossy().to_string();

    let engine = Arc::new(FrontendEngine::new("stela", data.join("frontend")).await?);

    // The stylesheet never changes between rebuilds, so it is pushed once.
    engine
        .update_asset("/style.css", STYLE_CSS.as_bytes().to_vec())
        .await?;

    // Render before serving: `/` has to answer 200 from the first request, even
    // with no posts, or a readiness probe never passes.
    rebuild(&engine, &posts_dir, &site).await?;

    log::warn!(
        "writes are UNAUTHENTICATED in this build — /api/posts and /admin/rebuild \
         are open to anyone who can reach the port. Session/RBAC lands with the \
         admin panel; do not expose this build."
    );
    log::info!("stela serving on http://{host}:{port}");

    let rebuild_engine = engine.clone();
    let rebuild_dir = posts_dir.clone();
    let rebuild_site = site.clone();
    let serve_engine = engine.clone();

    LithairServer::new()
        .with_port(port)
        .with_host(&host)
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
        .with_route(Method::GET, "/*".to_string(), move |req| {
            let engine = serve_engine.clone();
            Box::pin(async move { Ok(serve_page(req, &engine).await) })
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
        .update_asset("/index.html", tera.render("index.html", &ctx)?.into_bytes())
        .await?;
    engine
        .update_asset("/rss.xml", tera.render("rss.xml", &ctx)?.into_bytes())
        .await?;

    for post in &posts {
        let mut page = ctx.clone();
        page.insert("post", post);
        let slug = post["slug"].as_str().unwrap_or_default();
        let html = tera.render("post.html", &page)?;
        engine
            .update_asset(&format!("/posts/{slug}"), html.into_bytes())
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

/// Serve a rendered page out of memory.
///
/// Lithair's FrontendServer is bypassed here for one reason: it derives the
/// Content-Type from the path extension, and a blog's URLs have none
/// (`/posts/hello`), so every page would go out as application/octet-stream and
/// download instead of rendering. `update_asset` offers no way to set the type.
/// Since we rendered these pages, we know what they are, so we say so.
async fn serve_page(req: Request<hyper::body::Incoming>, engine: &FrontendEngine) -> Resp {
    let path = match req.uri().path() {
        "" | "/" => "/index.html",
        p => p,
    };

    match engine.get_asset(path).await {
        Some(asset) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", content_type(path))
            .body(Full::new(Bytes::from(asset.content)).boxed())
            .expect("response builder cannot fail on a static header set"),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(Full::new(Bytes::from("<h1>404</h1>")).boxed())
            .expect("response builder cannot fail on a static header set"),
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("css") => "text/css; charset=utf-8",
        Some("xml") => "application/xml; charset=utf-8",
        // Everything else we store is a rendered page, extension or not.
        _ => "text/html; charset=utf-8",
    }
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
    fn extensionless_post_urls_are_served_as_html() {
        // The whole reason serve_page exists rather than FrontendServer.
        assert_eq!(content_type("/posts/hello"), "text/html; charset=utf-8");
        assert_eq!(content_type("/index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type("/rss.xml"), "application/xml; charset=utf-8");
        assert_eq!(content_type("/style.css"), "text/css; charset=utf-8");
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
        assert_eq!(names, ["index.html", "post.html", "rss.xml"]);
    }
}
