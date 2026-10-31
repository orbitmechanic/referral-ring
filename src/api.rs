//! HTTP API. Thin: validates input, calls the domain/db layer, shapes JSON.

use crate::db::{self, NewCode};
use crate::domain::{self, ClaimState, MAX_NEEDS};
use axum::{
    extract::{ConnectInfo, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    /// Public base URL of the site, used to build notification bodies.
    pub site_url: String,
    /// Where push notifications go. Defaults to ntfy.sh; self-hostable.
    pub ntfy_base: String,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

pub struct ApiError(StatusCode, &'static str, String);

impl ApiError {
    fn new(code: StatusCode, kind: &'static str, msg: impl Into<String>) -> Self {
        Self(code, kind, msg.into())
    }
    fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", msg)
    }
    fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", msg)
    }
    fn conflict(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", msg)
    }
    fn limited() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many requests from this network — wait a few minutes",
        )
    }
    fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!("internal error: {e}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "something broke on our end")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1, "message": self.2 }))).into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct BinOut {
    pub need: u8,
    pub count: u32,
}

#[derive(Serialize)]
pub struct PoolResp {
    pub bins: Vec<BinOut>,
    pub total: u32,
}

#[derive(Serialize)]
pub struct DrawResp {
    pub code: String,
    pub need: u8,
    pub remaining: u8,
    pub expires_at: i64,
}

#[derive(Deserialize)]
pub struct DrawReq {
    #[serde(default)]
    #[allow(dead_code)]
    pub exclude: Vec<String>,
}

#[derive(Deserialize)]
pub struct ClaimReq {
    pub code: String,
    #[serde(default)]
    pub label: String,
    pub contact: String,
}

#[derive(Serialize)]
pub struct ClaimCreated {
    pub token: String,
    pub state: &'static str,
}

#[derive(Serialize)]
pub struct ClaimStatus {
    pub state: &'static str,
    pub label: String,
    pub referrer_code: String,
    pub referrer_label: String,
    pub can_submit: bool,
    pub my_code: Option<String>,
    pub created_at: i64,
}

#[derive(Deserialize)]
pub struct SubmitReq {
    pub code: String,
    pub need: i64,
    #[serde(default)]
    pub label: String,
    pub contact: String,
    #[serde(default)]
    pub notify_topic: Option<String>,
}

#[derive(Serialize)]
pub struct SubmitResp {
    /// Shown exactly once. This is the only key to the account.
    pub account_token: String,
    pub code: String,
    pub needs: u8,
}

#[derive(Serialize)]
pub struct LineOut {
    pub id: i64,
    pub code: String,
    pub label: String,
    pub need: u8,
    pub remaining: u8,
    pub status: String,
    pub confirmed: i64,
    pub rejected: i64,
    /// How many times this code has been dealt (transparency for the owner).
    pub served: i64,
    /// True while someone holds it in a live 72h reservation.
    pub held: bool,
    pub claims: Vec<ClaimOut>,
    pub sponsored: Vec<String>,
}

#[derive(Serialize)]
pub struct ClaimOut {
    pub id: i64,
    pub label: String,
    pub contact: String,
    pub state: String,
    pub created_at: i64,
    pub note: String,
}

#[derive(Serialize)]
pub struct ManageResp {
    pub label: String,
    pub contact: String,
    pub notify_topic: Option<String>,
    pub lines: Vec<LineOut>,
}

#[derive(Deserialize)]
pub struct ConfirmReq {
    pub claim_id: i64,
}

#[derive(Deserialize)]
pub struct RejectReq {
    pub claim_id: i64,
    #[serde(default)]
    pub note: String,
}

#[derive(Deserialize)]
pub struct LineReq {
    pub code: String,
    pub need: i64,
    #[serde(default)]
    pub label: String,
}

#[derive(Deserialize)]
pub struct NotifyReq {
    #[serde(default)]
    pub topic: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn client_key(addr: Option<&SocketAddr>, bucket: &str) -> String {
    let ip = addr.map(|a| a.ip().to_string()).unwrap_or_else(|| "unknown".into());
    format!("{bucket}:{ip}")
}

fn limit(
    conn: &rusqlite::Connection,
    addr: Option<&SocketAddr>,
    bucket: &str,
    window: i64,
    max: i64,
) -> Result<(), ApiError> {
    let ok = db::rate_ok(conn, &client_key(addr, bucket), db::now(), window, max)
        .map_err(ApiError::internal)?;
    if ok {
        Ok(())
    } else {
        Err(ApiError::limited())
    }
}

fn account_id_for(
    conn: &rusqlite::Connection,
    token: &str,
) -> Result<db::Account, ApiError> {
    db::account_by_token(conn, &domain::hash_token(token))
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("that link is not valid"))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "service": "referral-ring" }))
}

pub async fn pool(State(st): State<AppState>) -> ApiResult<PoolResp> {
    let conn = st.db.lock().unwrap();
    let bins = db::pool_summary(&conn, db::now()).map_err(ApiError::internal)?;
    let total = bins.iter().map(|b| b.1).sum();
    Ok(Json(PoolResp {
        bins: bins.iter().map(|(n, c)| BinOut { need: *n, count: *c }).collect(),
        total,
    }))
}

pub async fn draw(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: Option<Json<DrawReq>>,
) -> ApiResult<DrawResp> {
    let exclude = body.map(|b| b.0.exclude).unwrap_or_default();
    let conn = st.db.lock().unwrap();
    limit(&conn, Some(&addr), "draw", 3600, 60)?;

    let t = db::now();
    let _ = db::expire_stale_reservations(&conn, t);
    let _ = db::expire_stale_claims(&conn, t);

    let mut cands = db::candidates(&conn, t).map_err(ApiError::internal)?;
    if !exclude.is_empty() {
        // Don't show the same code twice in a row to the same browser.
        let kept: Vec<_> = cands
            .iter()
            .filter(|c| {
                db::code_by_id(&conn, c.id)
                    .ok()
                    .flatten()
                    .map(|code| !exclude.iter().any(|e| e.eq_ignore_ascii_case(&code.code)))
                    .unwrap_or(false)
            })
            .copied()
            .collect();
        if !kept.is_empty() {
            cands = kept;
        }
    }

    if cands.is_empty() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "pool_empty",
            "every code is signed up, in flight, or cooling down. Try again in a few minutes.",
        ));
    }

    // A few attempts in case two visitors race for the same row.
    for _ in 0..8 {
        let Some(id) = domain::pick_random(&cands) else {
            break;
        };
        if db::reserve(&conn, id, t).map_err(ApiError::internal)? {
            if let Some(code) = db::code_by_id(&conn, id).map_err(ApiError::internal)? {
                let _ = db::log_event(&conn, "draw", Some(id), None, &code.code, t);
                return Ok(Json(DrawResp {
                    code: code.code,
                    need: code.needs,
                    remaining: code.remaining,
                    expires_at: t + domain::RESERVE_SECS,
                }));
            }
        }
    }
    Err(ApiError::conflict("the pool shifted under us — pull again"))
}

pub async fn create_claim(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<ClaimReq>,
) -> ApiResult<ClaimCreated> {
    let conn = st.db.lock().unwrap();
    limit(&conn, Some(&addr), "claim", 3600, 15)?;

    let code_str = domain::normalize_code(&req.code)
        .ok_or_else(|| ApiError::bad("that referral code doesn't look right"))?;
    let contact = domain::clean_contact(&req.contact)
        .ok_or_else(|| ApiError::bad("leave a contact we can reach you at"))?;
    let label = domain::clean_label(&req.label);

    let code = db::code_by_string(&conn, &code_str)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("that code isn't in the ring (yet)"))?;

    if code.status != "active" || code.remaining == 0 {
        return Err(ApiError::conflict("that code is already fully rewarded — pull a different one"));
    }
    if db::open_claim_for(&conn, code.id).map_err(ApiError::internal)?.is_some() {
        return Err(ApiError::conflict(
            "someone already registered that code and is waiting on the same confirmation. \
             Pull a different code.",
        ));
    }

    let token = domain::new_token();
    let t = db::now();
    let claim_id = db::create_claim(
        &conn,
        code.id,
        &label,
        &contact,
        &domain::hash_token(&token),
        t,
    )
    .map_err(|e| {
        // unique index race -> someone else got there first
        if e.to_string().contains("UNIQUE") {
            ApiError::conflict("someone just claimed that code — pull a different one")
        } else {
            ApiError::internal(e)
        }
    })?;

    let _ = db::log_event(&conn, "claim_created", Some(code.id), Some(claim_id), "", t);
    notify_referrer(&st, &conn, code.account_id, &label, &code.code);

    Ok(Json(ClaimCreated { token, state: "pending" }))
}

pub async fn claim_status(
    State(st): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<ClaimStatus> {
    let conn = st.db.lock().unwrap();
    let claim = db::claim_by_token(&conn, &domain::hash_token(&token))
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("that link is not valid"))?;
    let code = db::code_by_id(&conn, claim.code_id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("that link is not valid"))?;

    let my_code = if let Some(child) = claim_child_code(&conn, claim.id)? {
        Some(child)
    } else {
        None
    };

    Ok(Json(ClaimStatus {
        state: claim.state.as_str(),
        label: claim.label,
        referrer_code: code.code,
        referrer_label: code.label,
        can_submit: claim.state.can_submit_own_code(),
        my_code,
        created_at: claim.created_at,
    }))
}

fn claim_child_code(conn: &rusqlite::Connection, claim_id: i64) -> Result<Option<String>, ApiError> {
    let child: Option<i64> = conn
        .query_row(
            "SELECT child_id FROM claims WHERE id = ?1",
            rusqlite::params![claim_id],
            |r| r.get(0),
        )
        .map_err(ApiError::internal)?;
    match child {
        Some(id) => Ok(db::code_by_id(conn, id).map_err(ApiError::internal)?.map(|c| c.code)),
        None => Ok(None),
    }
}

pub async fn submit_own_code(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(token): Path<String>,
    Json(req): Json<SubmitReq>,
) -> ApiResult<SubmitResp> {
    let mut conn = st.db.lock().unwrap();
    limit(&conn, Some(&addr), "submit", 3600, 15)?;

    let claim = db::claim_by_token(&conn, &domain::hash_token(&token))
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("that link is not valid"))?;

    if !claim.state.can_submit_own_code() {
        return Err(ApiError::conflict(match claim.state {
            ClaimState::Pending => {
                "the person who gave you that code hasn't confirmed it on their bill yet"
            }
            ClaimState::Completed => "you've already added your code from this claim",
            _ => "this claim is closed",
        }));
    }

    let code = domain::normalize_code(&req.code)
        .ok_or_else(|| ApiError::bad("that referral code doesn't look right"))?;
    let needs = domain::valid_needs(req.need)
        .ok_or_else(|| ApiError::bad("how many referrals do you still need (1-4)?"))?;
    let contact = domain::clean_contact(&req.contact)
        .ok_or_else(|| ApiError::bad("leave a contact we can reach you at"))?;
    let label = domain::clean_label(&req.label);
    let topic = req
        .notify_topic
        .as_deref()
        .map(str::trim)
        .filter(|t| domain::valid_topic(t));

    let account_token = domain::new_token();
    let done = db::complete_claim(
        &mut conn,
        claim.id,
        &label,
        &contact,
        &code,
        needs,
        &domain::hash_token(&account_token),
        topic,
        db::now(),
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            ApiError::conflict("that code is already in the ring — use a different one")
        } else {
            ApiError::internal(e)
        }
    })?
    .ok_or_else(|| ApiError::conflict("that code is already in the ring — use a different one"))?;

    let _ = db::log_event(
        &conn,
        "claim_completed",
        Some(done.code_id),
        Some(claim.id),
        &code,
        db::now(),
    );

    Ok(Json(SubmitResp {
        account_token,
        code,
        needs: MAX_NEEDS.min(needs),
    }))
}

pub async fn manage(
    State(st): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<ManageResp> {
    let conn = st.db.lock().unwrap();
    let acct = account_id_for(&conn, &token)?;
    let codes = db::codes_for_account(&conn, acct.id).map_err(ApiError::internal)?;
    let now = db::now();

    let mut lines = Vec::new();
    for c in codes {
        let claims = db::claims_for_code(&conn, c.id).map_err(ApiError::internal)?;
        let sponsored = db::downline(&conn, c.id)
            .map_err(ApiError::internal)?
            .into_iter()
            .map(|d| d.code)
            .collect();
        lines.push(LineOut {
            id: c.id,
            code: c.code,
            label: c.label,
            need: c.needs,
            remaining: c.remaining,
            status: c.status,
            confirmed: c.confirmed,
            rejected: c.rejected,
            served: c.served,
            held: c.reserved_until.map(|u| u > now).unwrap_or(false),
            claims: claims
                .into_iter()
                .map(|cl| ClaimOut {
                    id: cl.id,
                    label: cl.label,
                    contact: cl.contact,
                    state: cl.state.as_str().to_string(),
                    created_at: cl.created_at,
                    note: cl.note,
                })
                .collect(),
            sponsored,
        });
    }

    Ok(Json(ManageResp {
        label: acct.label,
        contact: acct.contact,
        notify_topic: acct.notify_topic,
        lines,
    }))
}

pub async fn confirm(
    State(st): State<AppState>,
    Path(token): Path<String>,
    Json(req): Json<ConfirmReq>,
) -> ApiResult<serde_json::Value> {
    let mut conn = st.db.lock().unwrap();
    let acct = account_id_for(&conn, &token)?;
    let claim = own_claim(&conn, &acct, req.claim_id)?;

    let ok = db::confirm_claim(&mut conn, claim.id, db::now()).map_err(ApiError::internal)?;
    if !ok {
        return Err(ApiError::conflict("that claim already moved on"));
    }
    let _ = db::log_event(&conn, "claim_confirmed", Some(claim.code_id), Some(claim.id), "", db::now());
    Ok(Json(serde_json::json!({ "ok": true, "state": "confirmed" })))
}

pub async fn reject(
    State(st): State<AppState>,
    Path(token): Path<String>,
    Json(req): Json<RejectReq>,
) -> ApiResult<serde_json::Value> {
    let mut conn = st.db.lock().unwrap();
    let acct = account_id_for(&conn, &token)?;
    let claim = own_claim(&conn, &acct, req.claim_id)?;

    let ok = db::reject_claim(&mut conn, claim.id, &domain::clean_label(&req.note), db::now())
        .map_err(ApiError::internal)?;
    if !ok {
        return Err(ApiError::conflict("that claim already moved on"));
    }
    let _ = db::log_event(&conn, "claim_rejected", Some(claim.code_id), Some(claim.id), &req.note, db::now());
    Ok(Json(serde_json::json!({ "ok": true, "state": "rejected" })))
}

fn own_claim(
    conn: &rusqlite::Connection,
    acct: &db::Account,
    claim_id: i64,
) -> Result<db::Claim, ApiError> {
    let codes = db::codes_for_account(conn, acct.id).map_err(ApiError::internal)?;
    let ids: Vec<i64> = codes.iter().map(|c| c.id).collect();
    for id in ids {
        for cl in db::claims_for_code(conn, id).map_err(ApiError::internal)? {
            if cl.id == claim_id {
                return Ok(cl);
            }
        }
    }
    Err(ApiError::not_found("no such claim on your lines"))
}

pub async fn add_line(
    State(st): State<AppState>,
    Path(token): Path<String>,
    Json(req): Json<LineReq>,
) -> ApiResult<serde_json::Value> {
    let conn = st.db.lock().unwrap();
    let acct = account_id_for(&conn, &token)?;

    let code = domain::normalize_code(&req.code)
        .ok_or_else(|| ApiError::bad("that referral code doesn't look right"))?;
    let needs = domain::valid_needs(req.need)
        .ok_or_else(|| ApiError::bad("how many referrals does this line still need (1-4)?"))?;
    let label = domain::clean_label(&req.label);

    if db::code_taken(&conn, &code).map_err(ApiError::internal)? {
        return Err(ApiError::conflict("that code is already in the ring"));
    }

    let t = db::now();
    db::insert_code(
        &conn,
        &NewCode {
            account_id: acct.id,
            parent_id: None,
            code: &code,
            label: &label,
            needs,
        },
        t,
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            ApiError::conflict("that code is already in the ring")
        } else {
            ApiError::internal(e)
        }
    })?;
    let _ = db::log_event(&conn, "line_added", None, None, &code, t);
    Ok(Json(serde_json::json!({ "ok": true, "code": code })))
}

pub async fn set_notify(
    State(st): State<AppState>,
    Path(token): Path<String>,
    Json(req): Json<NotifyReq>,
) -> ApiResult<serde_json::Value> {
    let conn = st.db.lock().unwrap();
    let acct = account_id_for(&conn, &token)?;

    let topic = req.topic.trim();
    if topic.is_empty() {
        db::set_notify_topic(&conn, acct.id, None).map_err(ApiError::internal)?;
        return Ok(Json(serde_json::json!({ "ok": true, "topic": null })));
    }
    if !domain::valid_topic(topic) {
        return Err(ApiError::bad("topic must be 4-64 letters, digits, dash or underscore"));
    }
    db::set_notify_topic(&conn, acct.id, Some(topic)).map_err(ApiError::internal)?;
    Ok(Json(serde_json::json!({ "ok": true, "topic": topic })))
}

/// Fire-and-forget push via ntfy.sh: no account, no API key, no email provider,
/// just a topic name the user subscribes to in a free app.
fn notify_referrer(
    st: &AppState,
    conn: &rusqlite::Connection,
    account_id: i64,
    claimant_label: &str,
    code: &str,
) {
    let acct = db::account_by_id(conn, account_id).ok().flatten();
    let Some(topic) = acct.and_then(|a| a.notify_topic) else {
        return;
    };
    let who = if claimant_label.is_empty() { "Someone" } else { claimant_label };
    let body = format!(
        "{who} used your code {code} and is waiting on you. Check your Cape bill: \
         if the $20 credit landed, confirm it here: {}/#/m/YOUR-LINK",
        st.site_url
    );
    let url = format!("{}/{}", st.ntfy_base.trim_end_matches('/'), topic);
    let client = reqwest::Client::new();
    tokio::spawn(async move {
        let res = client
            .post(url)
            .header("Title", "Referral Ring: a claim is waiting")
            .header("Tags", "phone")
            .header("Priority", "default")
            .body(body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;
        if let Err(e) = res {
            tracing::warn!("ntfy push failed: {e}");
        }
    });
}
