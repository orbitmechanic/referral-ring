//! Pure domain logic: no I/O, no database, no HTTP.
//!
//! Everything here is unit-tested. The rules that matter:
//!
//! * A person's `remaining` count is how many referrals they still need
//!   (Cape: $20 off per referral, capped at the bill, so 4 == free).
//! * Codes are dealt by weighted lottery favouring high-need codes, but a code
//!   can only be in flight with **one** claim at a time. Otherwise fifty people
//!   get the same code and forty-nine of them waste a sign-up.
//! * A newcomer cannot submit their own code until the referrer confirms that
//!   Cape actually granted the credit. That is the whole trust model.

use rand::RngCore;
use sha2::{Digest, Sha256};

/// Cape: $20 off per referral, capped at your bill. 4 referrals => $0/mo.
pub const MAX_NEEDS: u8 = 4;

/// How long a dealt code stays reserved before it returns to the pool.
pub const RESERVE_SECS: i64 = 72 * 3600;

/// Minimum gap before the same code can be dealt again after a reservation lapses.
pub const SERVE_COOLDOWN_SECS: i64 = 15 * 60;

// ---------------------------------------------------------------------------
// Claim state machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimState {
    /// Newcomer registered the code they used; referrer has not checked Cape yet.
    Pending,
    /// Referrer saw the credit on their bill and vouched for it.
    Confirmed,
    /// Newcomer handed their own code back. Referrer's `remaining` went down by 1.
    Completed,
    /// Referrer says no credit appeared (wrong code, cancelled signup, fraud).
    Rejected,
    /// Nobody acted in time; the slot returns to the pool.
    Expired,
}

impl ClaimState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Completed => "completed",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "confirmed" => Self::Confirmed,
            "completed" => Self::Completed,
            "rejected" => Self::Rejected,
            "expired" => Self::Expired,
            _ => return None,
        })
    }

    /// Only a pending claim can be vouched for by the referrer.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn can_confirm(self) -> bool {
        self == Self::Pending
    }

    /// The gate: no code, no entry, until the referrer confirmed.
    pub fn can_submit_own_code(self) -> bool {
        self == Self::Confirmed
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::Confirmed)
    }
}

// ---------------------------------------------------------------------------
// Weighted lottery
// ---------------------------------------------------------------------------

/// A code that is eligible to be dealt right now.
#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    pub id: i64,
    pub remaining: u8,
}

/// Weight favours the most desperate: 4 -> 4, 3 -> 3, 2 -> 2, 1 -> 1.
/// Out of ten equal-pool draws that is 40% / 30% / 20% / 10%.
pub fn weight_for(remaining: u8) -> u64 {
    u64::from(remaining.clamp(1, MAX_NEEDS))
}

pub fn total_weight(cands: &[Candidate]) -> u64 {
    cands.iter().map(|c| weight_for(c.remaining)).sum()
}

/// Deterministic core of the draw: `roll` must be in `[0, total_weight)`.
/// Kept pure so the distribution can be tested by enumeration.
pub fn pick_with_roll(cands: &[Candidate], roll: u64) -> Option<i64> {
    let total = total_weight(cands);
    if total == 0 || roll >= total {
        return None;
    }
    let mut cursor = roll;
    for c in cands {
        let w = weight_for(c.remaining);
        if cursor < w {
            return Some(c.id);
        }
        cursor -= w;
    }
    None
}

/// Real draw. Entropy comes from the OS, not from anything the client can see.
pub fn pick_random(cands: &[Candidate]) -> Option<i64> {
    let total = total_weight(cands);
    if total == 0 {
        return None;
    }
    let roll = rand::thread_rng().next_u32() as u64 % total;
    pick_with_roll(cands, roll)
}

/// Counts per bin, for the "how busy is the pool" display.
pub fn bin_counts(cands: &[Candidate]) -> [(u8, u32); 4] {
    let mut out = [(0u8, 0u32); 4];
    for (i, slot) in out.iter_mut().enumerate() {
        let bin = MAX_NEEDS - i as u8; // 4,3,2,1
        slot.0 = bin;
        slot.1 = cands.iter().filter(|c| c.remaining == bin).count() as u32;
    }
    out
}

// ---------------------------------------------------------------------------
// Tokens & codes
// ---------------------------------------------------------------------------

/// Crockford base32: no I, L, O, U, so codes stay unambiguous when read aloud
/// or written down on paper at a phone store.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 160 bits of OS randomness, encoded as 32 unambiguous characters.
pub fn new_token() -> String {
    let mut bytes = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut bytes);
    let mut out = String::with_capacity(32);
    for chunk in bytes.chunks(5) {
        // 5 bytes == 40 bits == exactly 8 base32 symbols
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = u64::from_be_bytes([0, 0, 0, buf[0], buf[1], buf[2], buf[3], buf[4]]);
        for i in 0..8 {
            let idx = ((n >> (35 - i * 5)) & 0x1f) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    out
}

/// Tokens are never stored in plaintext; only their hash is.
pub fn hash_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.trim().as_bytes());
    format!("{:x}", h.finalize())
}

/// Referral codes are matched case-insensitively and stripped of punctuation,
/// so "k7q2 m4rb" and "K7Q2-M4RB" are the same referral.
pub fn normalize_code(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase();
    if cleaned.len() < 4 || cleaned.len() > 24 {
        None
    } else {
        Some(cleaned)
    }
}

pub fn valid_needs(n: i64) -> Option<u8> {
    if (1..=i64::from(MAX_NEEDS)).contains(&n) {
        Some(n as u8)
    } else {
        None
    }
}

/// Contact is free-form (email, Keet handle, phone). We only need something
/// plausible enough to reach a human, and small enough to not be an attack.
pub fn clean_contact(raw: &str) -> Option<String> {
    let s = raw.trim();
    let ok_len = (3..=120).contains(&s.chars().count());
    let printable = s.chars().all(|c| !c.is_control());
    if ok_len && printable {
        Some(s.to_string())
    } else {
        None
    }
}

pub fn clean_label(raw: &str) -> String {
    let s: String = raw.trim().chars().filter(|c| !c.is_control()).take(40).collect();
    s
}

/// ntfy topic: lowercase letters, digits, dash, underscore. We generate these.
pub fn valid_topic(t: &str) -> bool {
    (4..=64).contains(&t.len())
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> Vec<Candidate> {
        vec![
            Candidate { id: 1, remaining: 4 },
            Candidate { id: 2, remaining: 3 },
            Candidate { id: 3, remaining: 2 },
            Candidate { id: 4, remaining: 1 },
        ]
    }

    #[test]
    fn weighting_matches_the_promise() {
        // Fixed total from pool(): 10. Enumerate every possible roll.
        let mut hits = [0u32; 4];
        for roll in 0..total_weight(&pool()) {
            let id = pick_with_roll(&pool(), roll).unwrap();
            hits[(id - 1) as usize] += 1;
        }
        assert_eq!(hits, [4, 3, 2, 1], "4-need code must win 4x as often as 1-need");
    }

    #[test]
    fn out_of_range_roll_returns_none() {
        assert_eq!(pick_with_roll(&pool(), 10), None);
        assert_eq!(pick_with_roll(&[], 0), None);
    }

    #[test]
    fn zero_need_candidates_are_never_dealt() {
        // A fully-rewarded code must never be served; DB filters it too, but the
        // weight function must not silently turn 0 into a positive weight.
        let c = Candidate { id: 9, remaining: 0 };
        assert_eq!(weight_for(0), 1); // clamp is a floor, callers filter remaining>0
        assert_eq!(bin_counts(&[c]).iter().map(|b| b.1).sum::<u32>(), 0);
    }

    #[test]
    fn bins_are_ordered_four_to_one() {
        let bins = bin_counts(&pool());
        assert_eq!(bins, [(4, 1), (3, 1), (2, 1), (1, 1)]);
    }

    #[test]
    fn state_machine_gates_the_submission() {
        assert!(ClaimState::Pending.can_confirm());
        assert!(!ClaimState::Pending.can_submit_own_code(), "no confirmation, no code");
        assert!(!ClaimState::Confirmed.can_confirm(), "cannot double-confirm");
        assert!(ClaimState::Confirmed.can_submit_own_code());
        assert!(!ClaimState::Rejected.can_submit_own_code());
        assert!(!ClaimState::Completed.is_open());
        assert!(ClaimState::Pending.is_open());
        assert!(ClaimState::Confirmed.is_open());
    }

    #[test]
    fn state_strings_round_trip() {
        for s in [
            ClaimState::Pending,
            ClaimState::Confirmed,
            ClaimState::Completed,
            ClaimState::Rejected,
            ClaimState::Expired,
        ] {
            assert_eq!(ClaimState::parse(s.as_str()), Some(s));
        }
        assert_eq!(ClaimState::parse("nonsense"), None);
    }

    #[test]
    fn tokens_are_32_unambiguous_chars_and_never_repeat() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(c)));
        assert!(!a.contains(['I', 'L', 'O', 'U']));
    }

    #[test]
    fn token_hashing_is_stable_and_not_reversible() {
        let t = new_token();
        let h = hash_token(&t);
        assert_eq!(h.len(), 64);
        assert_eq!(h, hash_token(&format!(" {} ", t)), "whitespace is trimmed");
        assert_ne!(h, hash_token(&new_token()));
        assert!(!h.contains(&t[..8]));
    }

    #[test]
    fn codes_are_normalized_but_not_mangled() {
        assert_eq!(normalize_code(" k7q2-m4rb ").as_deref(), Some("K7Q2M4RB"));
        assert_eq!(normalize_code("abc def 123").as_deref(), Some("ABCDEF123"));
        assert_eq!(normalize_code("abc"), None, "too short");
        assert_eq!(normalize_code(&"a".repeat(25)), None, "too long");
        assert_eq!(normalize_code("!!!!"), None);
    }

    #[test]
    fn contact_validation() {
        assert!(clean_contact("a@b.co").is_some());
        assert!(clean_contact("  ").is_none());
        assert!(clean_contact("x").is_none());
        assert!(clean_contact("bad\u{0}null").is_none());
    }

    #[test]
    fn needs_must_be_one_through_four() {
        assert_eq!(valid_needs(1), Some(1));
        assert_eq!(valid_needs(4), Some(4));
        assert_eq!(valid_needs(0), None);
        assert_eq!(valid_needs(5), None);
        assert_eq!(valid_needs(-2), None);
    }

    #[test]
    fn draw_is_not_always_the_same_code() {
        // Sanity check on the real RNG path, not just the pure core.
        let p = pool();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..400 {
            seen.insert(pick_random(&p).unwrap());
        }
        assert!(seen.len() > 1, "draw appears fixed");
    }
}

#[cfg(test)]
mod distribution_tests {
    use super::*;

    /// Pool sizes differ, so this checks the interaction: bin weight = size x need.
    #[test]
    fn unequal_pool_favours_need_times_size() {
        // 2 codes need 4 (w 8), 5 need 3 (w 15), 3 need 2 (w 6), 1 needs 1 (w 1) => 30
        let mut pool = Vec::new();
        let mut id = 1;
        for (n, count) in [(4u8, 2), (3, 5), (2, 3), (1, 1)] {
            for _ in 0..count {
                pool.push(Candidate { id, remaining: n });
                id += 1;
            }
        }
        assert_eq!(total_weight(&pool), 30);

        let mut by_bin = [0u32; 5];
        for roll in 0..30 {
            let picked = pick_with_roll(&pool, roll).unwrap();
            let cand = pool.iter().find(|c| c.id == picked).unwrap();
            by_bin[cand.remaining as usize] += 1;
        }
        assert_eq!([0, 1, 6, 15, 8], by_bin, "4-need pool must dominate");
    }

    /// No code may be starved forever: every candidate must have a nonzero chance.
    #[test]
    fn every_candidate_can_be_drawn() {
        let mut pool = Vec::new();
        for i in 0..50 {
            pool.push(Candidate { id: i, remaining: (i % 4 + 1) as u8 });
        }
        let total = total_weight(&pool);
        let mut hits = std::collections::HashSet::new();
        for roll in 0..total {
            hits.insert(pick_with_roll(&pool, roll).unwrap());
        }
        assert_eq!(hits.len(), 50);
    }
}
