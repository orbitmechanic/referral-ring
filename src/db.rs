//! SQLite persistence. All invariants that can live in the database do live in
//! the database, so a bug in a handler cannot corrupt the ring.

use crate::domain::{self, Candidate, ClaimState, RESERVE_SECS, SERVE_COOLDOWN_SECS};
use rusqlite::{params, Connection, OptionalExtension};

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn open(path: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        PRAGMA busy_timeout = 5000;
        PRAGMA synchronous = NORMAL;

        CREATE TABLE IF NOT EXISTS accounts (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            label        TEXT    NOT NULL DEFAULT '',
            contact      TEXT    NOT NULL,
            token_hash   TEXT    NOT NULL UNIQUE,
            notify_topic TEXT,
            created_at   INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS codes (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            parent_id      INTEGER REFERENCES codes(id),
            code           TEXT    NOT NULL UNIQUE COLLATE NOCASE,
            label          TEXT    NOT NULL DEFAULT '',
            needs          INTEGER NOT NULL,
            remaining      INTEGER NOT NULL,
            status         TEXT    NOT NULL DEFAULT 'active',
            served         INTEGER NOT NULL DEFAULT 0,
            last_served_at INTEGER,
            reserved_until INTEGER,
            confirmed      INTEGER NOT NULL DEFAULT 0,
            rejected       INTEGER NOT NULL DEFAULT 0,
            created_at     INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS codes_bin ON codes(status, remaining);

        CREATE TABLE IF NOT EXISTS claims (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            code_id      INTEGER NOT NULL REFERENCES codes(id) ON DELETE CASCADE,
            label        TEXT    NOT NULL DEFAULT '',
            contact      TEXT    NOT NULL,
            token_hash   TEXT    NOT NULL UNIQUE,
            state        TEXT    NOT NULL,
            note         TEXT    NOT NULL DEFAULT '',
            child_id     INTEGER REFERENCES codes(id),
            created_at   INTEGER NOT NULL,
            decided_at   INTEGER,
            completed_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS claims_code ON claims(code_id, state);

        -- THE invariant: a code is never handed to two people at once.
        CREATE UNIQUE INDEX IF NOT EXISTS claims_one_open_per_code
            ON claims(code_id) WHERE state IN ('pending', 'confirmed');

        CREATE TABLE IF NOT EXISTS events (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            at       INTEGER NOT NULL,
            kind     TEXT    NOT NULL,
            code_id  INTEGER,
            claim_id INTEGER,
            detail   TEXT    NOT NULL DEFAULT ''
        );

        CREATE TABLE IF NOT EXISTS rate (
            key          TEXT PRIMARY KEY,
            window_start INTEGER NOT NULL,
            count        INTEGER NOT NULL
        );
        "#,
    )?;
    Ok(conn)
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Account {
    pub id: i64,
    pub label: String,
    pub contact: String,
    pub notify_topic: Option<String>,
    #[allow(dead_code)] // kept for the admin/stats path
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct Code {
    pub id: i64,
    pub account_id: i64,
    pub code: String,
    pub label: String,
    pub needs: u8,
    pub remaining: u8,
    pub status: String,
    pub served: i64,
    pub confirmed: i64,
    pub rejected: i64,
    pub reserved_until: Option<i64>,
    #[allow(dead_code)] // schema history, surfaced by `stats`
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct Claim {
    pub id: i64,
    pub code_id: i64,
    pub label: String,
    pub contact: String,
    pub state: ClaimState,
    pub note: String,
    pub created_at: i64,
}

fn map_code(r: &rusqlite::Row<'_>) -> rusqlite::Result<Code> {
    Ok(Code {
        id: r.get(0)?,
        account_id: r.get(1)?,
        code: r.get(2)?,
        label: r.get(3)?,
        needs: r.get::<_, i64>(4)? as u8,
        remaining: r.get::<_, i64>(5)? as u8,
        status: r.get(6)?,
        served: r.get(7)?,
        confirmed: r.get(8)?,
        rejected: r.get(9)?,
        reserved_until: r.get(10)?,
        created_at: r.get(11)?,
    })
}

const CODE_COLS: &str = "id, account_id, code, label, needs, remaining, status, served, \
                         confirmed, rejected, reserved_until, created_at";

fn map_claim(r: &rusqlite::Row<'_>) -> rusqlite::Result<Claim> {
    let state: String = r.get(4)?;
    Ok(Claim {
        id: r.get(0)?,
        code_id: r.get(1)?,
        label: r.get(2)?,
        contact: r.get(3)?,
        state: ClaimState::parse(&state).unwrap_or(ClaimState::Expired),
        note: r.get(5)?,
        created_at: r.get(6)?,
    })
}

const CLAIM_COLS: &str = "id, code_id, label, contact, state, note, created_at";

// ---------------------------------------------------------------------------
// Pool
// ---------------------------------------------------------------------------

/// Codes eligible to be dealt right now.
pub fn candidates(conn: &Connection, t: i64) -> rusqlite::Result<Vec<Candidate>> {
    let mut stmt = conn.prepare(
        "SELECT id, remaining FROM codes c
          WHERE c.status = 'active'
            AND c.remaining > 0
            AND (c.reserved_until IS NULL OR c.reserved_until <= ?1)
            AND (c.last_served_at IS NULL OR c.last_served_at <= ?2)
            AND NOT EXISTS (
                  SELECT 1 FROM claims cl
                   WHERE cl.code_id = c.id AND cl.state IN ('pending','confirmed')
            )
          ORDER BY c.id",
    )?;
    let rows = stmt.query_map(params![t, t - SERVE_COOLDOWN_SECS], |r| {
        Ok(Candidate {
            id: r.get(0)?,
            remaining: r.get::<_, i64>(1)? as u8,
        })
    })?;
    rows.collect()
}

/// Atomic reservation: only succeeds if the row is still free.
pub fn reserve(conn: &Connection, code_id: i64, t: i64) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "UPDATE codes
            SET reserved_until = ?2, served = served + 1, last_served_at = ?3
          WHERE id = ?1
            AND status = 'active'
            AND remaining > 0
            AND (reserved_until IS NULL OR reserved_until <= ?3)",
        params![code_id, t + RESERVE_SECS, t],
    )?;
    Ok(n == 1)
}

pub fn code_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<Code>> {
    conn.query_row(
        &format!("SELECT {CODE_COLS} FROM codes WHERE id = ?1"),
        params![id],
        map_code,
    )
    .optional()
}

pub fn pool_summary(conn: &Connection, t: i64) -> rusqlite::Result<[(u8, u32); 4]> {
    let cands = candidates(conn, t)?;
    Ok(domain::bin_counts(&cands))
}

// ---------------------------------------------------------------------------
// Accounts & codes
// ---------------------------------------------------------------------------

pub fn create_account(
    conn: &Connection,
    label: &str,
    contact: &str,
    token_hash: &str,
    notify_topic: Option<&str>,
    t: i64,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO accounts (label, contact, token_hash, notify_topic, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![label, contact, token_hash, notify_topic, t],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn account_by_token(conn: &Connection, token_hash: &str) -> rusqlite::Result<Option<Account>> {
    conn.query_row(
        "SELECT id, label, contact, notify_topic, created_at
           FROM accounts WHERE token_hash = ?1",
        params![token_hash],
        |r| {
            Ok(Account {
                id: r.get(0)?,
                label: r.get(1)?,
                contact: r.get(2)?,
                notify_topic: r.get(3)?,
                created_at: r.get(4)?,
            })
        },
    )
    .optional()
}

pub fn account_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<Account>> {
    conn.query_row(
        "SELECT id, label, contact, notify_topic, created_at FROM accounts WHERE id = ?1",
        params![id],
        |r| {
            Ok(Account {
                id: r.get(0)?,
                label: r.get(1)?,
                contact: r.get(2)?,
                notify_topic: r.get(3)?,
                created_at: r.get(4)?,
            })
        },
    )
    .optional()
}

pub fn set_notify_topic(
    conn: &Connection,
    account_id: i64,
    topic: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE accounts SET notify_topic = ?2 WHERE id = ?1",
        params![account_id, topic],
    )?;
    Ok(())
}

pub struct NewCode<'a> {
    pub account_id: i64,
    pub parent_id: Option<i64>,
    pub code: &'a str,
    pub label: &'a str,
    pub needs: u8,
}

pub fn insert_code(conn: &Connection, c: &NewCode<'_>, t: i64) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO codes (account_id, parent_id, code, label, needs, remaining,
                            status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'active', ?6)",
        params![c.account_id, c.parent_id, c.code, c.label, c.needs, t],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn code_by_string(conn: &Connection, code: &str) -> rusqlite::Result<Option<Code>> {
    conn.query_row(
        &format!("SELECT {CODE_COLS} FROM codes WHERE code = ?1 COLLATE NOCASE"),
        params![code],
        map_code,
    )
    .optional()
}

pub fn code_taken(conn: &Connection, code: &str) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM codes WHERE code = ?1 COLLATE NOCASE",
        params![code],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn codes_for_account(conn: &Connection, account_id: i64) -> rusqlite::Result<Vec<Code>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CODE_COLS} FROM codes WHERE account_id = ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map(params![account_id], map_code)?;
    rows.collect()
}

pub fn downline(conn: &Connection, code_id: i64) -> rusqlite::Result<Vec<Code>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CODE_COLS} FROM codes WHERE parent_id = ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map(params![code_id], map_code)?;
    rows.collect()
}

// ---------------------------------------------------------------------------
// Claims
// ---------------------------------------------------------------------------

pub fn open_claim_for(conn: &Connection, code_id: i64) -> rusqlite::Result<Option<Claim>> {
    conn.query_row(
        &format!(
            "SELECT {CLAIM_COLS} FROM claims
              WHERE code_id = ?1 AND state IN ('pending','confirmed') LIMIT 1"
        ),
        params![code_id],
        map_claim,
    )
    .optional()
}

pub fn create_claim(
    conn: &Connection,
    code_id: i64,
    label: &str,
    contact: &str,
    token_hash: &str,
    t: i64,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO claims (code_id, label, contact, token_hash, state, created_at)
         VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
        params![code_id, label, contact, token_hash, t],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn claim_by_token(conn: &Connection, token_hash: &str) -> rusqlite::Result<Option<Claim>> {
    conn.query_row(
        &format!("SELECT {CLAIM_COLS} FROM claims WHERE token_hash = ?1"),
        params![token_hash],
        map_claim,
    )
    .optional()
}

pub fn claims_for_code(conn: &Connection, code_id: i64) -> rusqlite::Result<Vec<Claim>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CLAIM_COLS} FROM claims WHERE code_id = ?1 ORDER BY id DESC"
    ))?;
    let rows = stmt.query_map(params![code_id], map_claim)?;
    rows.collect()
}

/// Compare-and-set transition. Returns false if the claim was not in `from`.
pub fn transition(
    conn: &Connection,
    claim_id: i64,
    from: ClaimState,
    to: ClaimState,
    t: i64,
) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "UPDATE claims SET state = ?3, decided_at = ?4 WHERE id = ?1 AND state = ?2",
        params![claim_id, from.as_str(), to.as_str(), t],
    )?;
    Ok(n == 1)
}

pub fn set_claim_note(conn: &Connection, claim_id: i64, note: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE claims SET note = ?2 WHERE id = ?1", params![claim_id, note])?;
    Ok(())
}

/// Referrer vouched that Cape granted the credit.
pub fn confirm_claim(conn: &mut Connection, claim_id: i64, t: i64) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    let ok = transition(&tx, claim_id, ClaimState::Pending, ClaimState::Confirmed, t)?;
    if ok {
        let code_id: i64 = tx.query_row(
            "SELECT code_id FROM claims WHERE id = ?1",
            params![claim_id],
            |r| r.get(0),
        )?;
        tx.execute("UPDATE codes SET confirmed = confirmed + 1 WHERE id = ?1", params![code_id])?;
    }
    tx.commit()?;
    Ok(ok)
}

/// Referrer says no credit appeared; the slot returns to circulation immediately.
pub fn reject_claim(
    conn: &mut Connection,
    claim_id: i64,
    note: &str,
    t: i64,
) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    let ok = transition(&tx, claim_id, ClaimState::Pending, ClaimState::Rejected, t)?;
    if ok {
        set_claim_note(&tx, claim_id, note)?;
        let code_id: i64 = tx.query_row(
            "SELECT code_id FROM claims WHERE id = ?1",
            params![claim_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE codes SET rejected = rejected + 1, reserved_until = NULL WHERE id = ?1",
            params![code_id],
        )?;
    }
    tx.commit()?;
    Ok(ok)
}

pub struct Completed {
    #[allow(dead_code)]
    pub account_id: i64,
    pub code_id: i64,
    #[allow(dead_code)] // asserted in tests, useful in the audit log
    pub referrer_remaining: u8,
}

/// The payoff. Runs as one transaction:
/// 1. the claim must be `confirmed` (the trust gate),
/// 2. a new account + code is created for the newcomer,
/// 3. the referrer's `remaining` drops by one,
/// 4. the referrer's code retires at zero,
/// 5. the claim closes and points at the new code.
pub fn complete_claim(
    conn: &mut Connection,
    claim_id: i64,
    label: &str,
    contact: &str,
    new_code: &str,
    needs: u8,
    token_hash: &str,
    notify_topic: Option<&str>,
    t: i64,
) -> rusqlite::Result<Option<Completed>> {
    let tx = conn.transaction()?;

    let (code_id, state): (i64, String) = tx.query_row(
        "SELECT code_id, state FROM claims WHERE id = ?1",
        params![claim_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if ClaimState::parse(&state).map(|s| s.can_submit_own_code()) != Some(true) {
        return Ok(None);
    }

    // Someone else may already hold this referral code string.
    let dup: i64 = tx.query_row(
        "SELECT COUNT(*) FROM codes WHERE code = ?1 COLLATE NOCASE",
        params![new_code],
        |r| r.get(0),
    )?;
    if dup > 0 {
        return Ok(None);
    }

    let account_id = create_account(&tx, label, contact, token_hash, notify_topic, t)?;
    let child = insert_code(
        &tx,
        &NewCode {
            account_id,
            parent_id: Some(code_id),
            code: new_code,
            label,
            needs,
        },
        t,
    )?;

    tx.execute(
        "UPDATE codes SET remaining = remaining - 1,
                          status = CASE WHEN remaining - 1 <= 0 THEN 'exhausted' ELSE 'active' END,
                          reserved_until = NULL
          WHERE id = ?1",
        params![code_id],
    )?;

    tx.execute(
        "UPDATE claims SET state = 'completed', completed_at = ?2, child_id = ?3,
                           decided_at = COALESCE(decided_at, ?2)
          WHERE id = ?1",
        params![claim_id, t, child],
    )?;

    let remaining: i64 = tx.query_row(
        "SELECT remaining FROM codes WHERE id = ?1",
        params![code_id],
        |r| r.get(0),
    )?;

    tx.commit()?;
    Ok(Some(Completed {
        account_id,
        code_id: child,
        referrer_remaining: remaining as u8,
    }))
}

/// A pending claim nobody acts on must not squat a code forever.
pub const CLAIM_TTL_SECS: i64 = 7 * 24 * 3600;

pub fn expire_stale_claims(conn: &Connection, t: i64) -> rusqlite::Result<usize> {
    let cutoff = t - CLAIM_TTL_SECS;
    let n = conn.execute(
        "UPDATE claims SET state = 'expired', decided_at = ?1
          WHERE state = 'pending' AND created_at <= ?2",
        params![t, cutoff],
    )?;
    conn.execute(
        "UPDATE codes SET reserved_until = NULL
          WHERE id IN (SELECT code_id FROM claims WHERE state = 'expired' AND decided_at = ?1)",
        params![t],
    )?;
    Ok(n)
}

/// Release reservations whose 72h window elapsed with no claim filed.
pub fn expire_stale_reservations(conn: &Connection, t: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE codes SET reserved_until = NULL
          WHERE reserved_until IS NOT NULL AND reserved_until <= ?1",
        params![t],
    )
}

// ---------------------------------------------------------------------------
// Rate limiting & audit
// ---------------------------------------------------------------------------

/// Fixed-window limiter keyed by (client, bucket). Returns true if allowed.
pub fn rate_ok(
    conn: &Connection,
    key: &str,
    t: i64,
    window: i64,
    limit: i64,
) -> rusqlite::Result<bool> {
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT window_start, count FROM rate WHERE key = ?1",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    let allowed = match row {
        Some((start, count)) if t - start < window => {
            if count >= limit {
                false
            } else {
                conn.execute("UPDATE rate SET count = count + 1 WHERE key = ?1", params![key])?;
                true
            }
        }
        _ => {
            conn.execute(
                "INSERT INTO rate (key, window_start, count) VALUES (?1, ?2, 1)
                 ON CONFLICT(key) DO UPDATE SET window_start = ?2, count = 1",
                params![key, t],
            )?;
            true
        }
    };
    Ok(allowed)
}

pub fn log_event(
    conn: &Connection,
    kind: &str,
    code_id: Option<i64>,
    claim_id: Option<i64>,
    detail: &str,
    t: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO events (at, kind, code_id, claim_id, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![t, kind, code_id, claim_id, detail],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        open(":memory:").unwrap()
    }

    fn seed(conn: &Connection, code: &str, needs: u8, t: i64) -> (i64, i64) {
        let acct = create_account(conn, "t", "t@t.co", &format!("h{code}"), None, t).unwrap();
        let id = insert_code(
            conn,
            &NewCode { account_id: acct, parent_id: None, code, label: "", needs },
            t,
        )
        .unwrap();
        (acct, id)
    }

    #[test]
    fn reserve_is_exclusive() {
        let conn = mem();
        let (_, id) = seed(&conn, "AAAA", 2, 1000);
        assert!(reserve(&conn, id, 1000).unwrap());
        // second pull inside the window must fail
        assert!(!reserve(&conn, id, 1000).unwrap());
        // after the reservation lapses it is dealable again
        assert!(reserve(&conn, id, 1000 + RESERVE_SECS + SERVE_COOLDOWN_SECS + 1).unwrap());
    }

    #[test]
    fn codes_with_open_claims_are_not_dealt() {
        let conn = mem();
        let (_, id) = seed(&conn, "BBBB", 4, 1000);
        let t = 1000 + RESERVE_SECS + SERVE_COOLDOWN_SECS + 10;
        create_claim(&conn, id, "x", "x@y.co", "h1", t).unwrap();
        let c = candidates(&conn, t).unwrap();
        assert!(c.is_empty(), "a code in flight must not be dealt again");
    }

    #[test]
    fn one_open_claim_per_code_is_enforced_by_the_index() {
        let conn = mem();
        let (_, id) = seed(&conn, "CCCC", 3, 1000);
        create_claim(&conn, id, "a", "a@b.co", "h1", 1000).unwrap();
        let second = create_claim(&conn, id, "b", "b@c.co", "h2", 1000);
        assert!(second.is_err(), "partial unique index should reject this");
    }

    #[test]
    fn full_cycle_decrements_and_retires() {
        let mut conn = mem();
        let (_, ref_code) = seed(&conn, "REF1", 1, 1000);
        let claim = create_claim(&conn, ref_code, "newbie", "n@x.co", "ct", 1100).unwrap();

        // cannot complete before confirming
        assert!(complete_claim(&mut conn, claim, "n", "n@x.co", "NEW1", 2, "nh", None, 1200)
            .unwrap()
            .is_none());

        assert!(confirm_claim(&mut conn, claim, 1150).unwrap());
        let done = complete_claim(&mut conn, claim, "n", "n@x.co", "NEW1", 2, "nh", None, 1200)
            .unwrap()
            .expect("confirmed claim must complete");
        assert_eq!(done.referrer_remaining, 0);

        let referrer = code_by_id(&conn, ref_code).unwrap().unwrap();
        assert_eq!(referrer.remaining, 0);
        assert_eq!(referrer.status, "exhausted", "a free line retires");

        let child = code_by_id(&conn, done.code_id).unwrap().unwrap();
        assert_eq!(child.remaining, 2);
        assert_eq!(child.status, "active");
        assert!(candidates(&conn, 1300).unwrap().iter().any(|c| c.id == child.id));
    }

    #[test]
    fn completing_twice_is_impossible() {
        let mut conn = mem();
        let (_, ref_code) = seed(&conn, "REF2", 3, 1000);
        let claim = create_claim(&conn, ref_code, "n", "n@x.co", "ct", 1100).unwrap();
        confirm_claim(&mut conn, claim, 1100).unwrap();
        assert!(complete_claim(&mut conn, claim, "n", "n@x.co", "NEW2", 1, "nh", None, 1200)
            .unwrap()
            .is_some());
        assert!(complete_claim(&mut conn, claim, "n", "n@x.co", "NEW3", 1, "nh2", None, 1300)
            .unwrap()
            .is_none());
        assert_eq!(code_by_id(&conn, ref_code).unwrap().unwrap().remaining, 2);
    }

    #[test]
    fn duplicate_code_strings_are_refused() {
        let mut conn = mem();
        let (_, a) = seed(&conn, "SAME", 2, 1000);
        let claim = create_claim(&conn, a, "n", "n@x.co", "ct", 1000).unwrap();
        confirm_claim(&mut conn, claim, 1000).unwrap();
        assert!(complete_claim(&mut conn, claim, "n", "n@x.co", "same", 1, "nh", None, 1000)
            .unwrap()
            .is_none(), "codes are case-insensitive unique");
    }

    #[test]
    fn reject_returns_the_code_to_the_pool() {
        let mut conn = mem();
        let (_, c) = seed(&conn, "REJ1", 4, 1000);
        let claim = create_claim(&conn, c, "n", "n@x.co", "ct", 1000).unwrap();
        assert!(reject_claim(&mut conn, claim, "no credit seen", 1001).unwrap());
        let row = code_by_id(&conn, c).unwrap().unwrap();
        assert_eq!(row.remaining, 4, "rejection must not cost the referrer");
        assert!(candidates(&conn, 1001 + SERVE_COOLDOWN_SECS + 1)
            .unwrap()
            .iter()
            .any(|x| x.id == c));
    }

    #[test]
    fn rate_limiter_resets_after_window() {
        let conn = mem();
        assert!(rate_ok(&conn, "ip1", 1000, 600, 2).unwrap());
        assert!(rate_ok(&conn, "ip1", 1000, 600, 2).unwrap());
        assert!(!rate_ok(&conn, "ip1", 1000, 600, 2).unwrap());
        assert!(rate_ok(&conn, "ip1", 1601, 600, 2).unwrap());
        assert!(rate_ok(&conn, "ip2", 1000, 600, 2).unwrap(), "other clients unaffected");
    }
}
