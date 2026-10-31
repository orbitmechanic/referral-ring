//! referral-ring — a referral code exchange where trust is mutual and explicit.
//!
//! Run:
//!   referral-ring serve                 # API + static site on :8080
//!   referral-ring add-code --code ABC123 --needs 2 --contact you@example.com
//!   referral-ring stats

mod api;
mod db;
mod domain;

use axum::{
    http::Method,
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(env_or("RUST_LOG", "referral_ring=info,tower_http=info"))
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("serve");

    match cmd {
        "serve" => serve().await,
        "add-code" => add_code(&args[1..]),
        "stats" => stats(),
        other => {
            eprintln!("unknown command: {other}\n\ncommands: serve | add-code | stats");
            std::process::exit(2);
        }
    }
}

fn db_path() -> String {
    env_or("DB_PATH", "ring.db")
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

// ---------------------------------------------------------------------------
// serve
// ---------------------------------------------------------------------------

async fn serve() {
    let db_path = db_path();
    let port: u16 = env_or("PORT", "8080").parse().unwrap_or(8080);
    let static_dir = env_or("STATIC_DIR", "web");
    let site_url = env_or("SITE_URL", &format!("http://localhost:{port}"));

    let conn = db::open(&db_path).expect("open database");
    let state = api::AppState {
        db: Arc::new(Mutex::new(conn)),
        site_url: site_url.clone(),
        ntfy_base: env_or("NTFY_BASE", "https://ntfy.sh"),
    };

    let index = format!("{static_dir}/index.html");
    let spa = ServeDir::new(&static_dir).fallback(ServeFile::new(index));

    let app = Router::new()
        .route("/api/health", get(api::health))
        .route("/api/pool", get(api::pool))
        .route("/api/draw", post(api::draw))
        .route("/api/claims", post(api::create_claim))
        .route("/api/claims/{token}", get(api::claim_status))
        .route("/api/claims/{token}/submit", post(api::submit_own_code))
        .route("/api/manage/{token}", get(api::manage))
        .route("/api/manage/{token}/confirm", post(api::confirm))
        .route("/api/manage/{token}/reject", post(api::reject))
        .route("/api/manage/{token}/lines", post(api::add_line))
        .route("/api/manage/{token}/notify", post(api::set_notify))
        .with_state(state)
        .fallback_service(spa)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("referral-ring listening on http://{addr} (db: {db_path})");

    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await
    .expect("serve");
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}

// ---------------------------------------------------------------------------
// add-code: bootstrap a line (you have no referrer for your own first code)
// ---------------------------------------------------------------------------

fn add_code(args: &[String]) {
    let Some(code_raw) = flag(args, "--code") else {
        eprintln!("usage: add-code --code CODE --needs 1..4 --contact EMAIL [--label NAME] [--notify TOPIC]");
        std::process::exit(2);
    };
    let Some(contact) = flag(args, "--contact") else {
        eprintln!("--contact is required");
        std::process::exit(2);
    };
    let needs: i64 = flag(args, "--needs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let label = flag(args, "--label").unwrap_or_default();
    let notify = flag(args, "--notify");

    let code = domain::normalize_code(&code_raw).expect("invalid code");
    let needs = domain::valid_needs(needs).expect("needs must be 1-4");
    let contact = domain::clean_contact(&contact).expect("invalid contact");

    let conn = db::open(&db_path()).expect("open database");
    if db::code_taken(&conn, &code).expect("db") {
        eprintln!("code {code} already exists");
        std::process::exit(1);
    }
    let token = domain::new_token();
    let t = db::now();
    let account = db::create_account(
        &conn,
        &domain::clean_label(&label),
        &contact,
        &domain::hash_token(&token),
        notify.as_deref(),
        t,
    )
    .expect("create account");
    db::insert_code(
        &conn,
        &db::NewCode {
            account_id: account,
            parent_id: None,
            code: &code,
            label: &domain::clean_label(&label),
            needs,
        },
        t,
    )
    .expect("create code");
    db::log_event(&conn, "seeded", None, None, &code, t).expect("log");

    let base = env_or("SITE_URL", "http://localhost:8080");
    println!("line added: {code} (needs {needs})");
    println!("\n  your private management link (keep it, it is the only key):\n");
    println!("  {base}/#/m/{token}\n");
}

fn stats() {
    let conn = db::open(&db_path()).expect("open database");
    let bins = db::pool_summary(&conn, db::now()).expect("query");
    println!("dealtime codes by need:");
    for (need, count) in bins {
        println!("  {need} referrals needed: {count}");
    }
}
