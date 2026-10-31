# Referral Ring

A referral-code exchange where **trust is a two-sided handshake** instead of a
leap of faith.

Pull a stranger's code → sign up → the code's owner checks their own bill and
vouches that the credit landed → *only then* do you get to put your own code
into the pool. The ring turns forever.

Built for [Cape](https://www.cape.co/referrals), whose terms give **both** sides
$20/month off for as long as both stay subscribed, capped at the bill — so four
referrals is a free line. Cape's own copy explicitly invites sharing codes with
"even strangers online". This is that, with the awkward part (finding a stranger,
and trusting them) handled.

---

## The one hard constraint

**GitHub Pages cannot store a database.** It is a CDN serving static files; there
is no process to hold state or accept a write. Any design that "commits to git
from the browser" means shipping a write-capable token inside the JavaScript,
where anyone can read it, and it cannot be un-leaked. So:

| Piece | Where | Why |
|---|---|---|
| UI (HTML/CSS/JS) | GitHub Pages | free, global, instant |
| API + database | one small Rust binary + SQLite | needs a process; see *Deploy* |
| Draw randomness | server | a client-side draw is a rigged draw |
| Notifications | ntfy.sh push | no account, no email provider, no SMTP |

## Why not Bevy

Bevy is a game engine: an ECS and a render loop. It has **no database** — that
part of the original plan doesn't exist — and a Bevy WASM build is ~10 MB of
renderer to animate three reels next to a form. The slot machine here is canvas
and CSS: the *reels* are theatre, and the draw that matters happens on the
server anyway. Bevy would be the right call if this were a game. It is a
database with a game-shaped front door, so the Rust went where Rust earns its
keep: the server.

---

## The state machine

```
              pull                     sign up at cape.co
  pool ─────────────► reserved ──────────────┐
   ▲                  72h                   │ newcomer files a claim
   │                                          ▼
   │                                    ┌──────────┐
   │  reject / 7-day expiry             │ pending  │  owner checks their bill
   └────────────────────────────────────┴────┬─────┘
                                              │ owner: "credit landed"
                                              ▼
                                         ┌───────────┐
                                         │ confirmed │  ◄── THE GATE
                                         └─────┬─────┘
                                               │ newcomer submits own code
                                               ▼
                                         ┌───────────┐
                                         │ completed │  owner's remaining -1
                                         └───────────┘
```

Invariants, enforced in SQL, not just in handlers:

- **One open claim per code** — `UNIQUE INDEX ... WHERE state IN ('pending','confirmed')`.
  Fifty people can never be handed the same code.
- **No submission before confirmation** — `ClaimState::can_submit_own_code()`.
- **A deal reserves the code for 72 h**, then it returns to the pool untouched.
- **A completed referral decrements exactly once** — compare-and-set transition
  inside a single transaction; replaying the request is a 409.
- **Codes retire at zero.**

## Dealing

Codes sit in bins by how many referrals they still need (4, 3, 2, 1). A draw
picks with weight `= remaining`, so a 4-need code is four times likelier than a
1-need code out of an otherwise equal pool. Exhaustively tested by enumerating
every possible roll:

```
equal pool (1,1,1,1):  40% / 30% / 20% / 10%
mixed pool:            weights are pool_count × need, never zero for a live code
```

A code with an open claim, a live reservation, or a recent serve is filtered out
before the roll, so the "needing 4" bin never starves when it's the only bin with
anything in it.

---

## Run it

```bash
cargo test                 # 22 tests: domain, state machine, SQL invariants
cargo run -- serve         # http://localhost:8080  (serves UI + API)
```

Seed your own lines first — you have no referrer for your own code:

```bash
./target/debug/referral-ring add-code \
  --code K7Q2M4RB --needs 1 --contact you@example.com --label "my line"
```

It prints a management link. That link **is** your identity — there are no
accounts and no passwords, so there is no reset. Bookmark it.

Env: `DB_PATH` (default `ring.db`), `PORT` (8080), `STATIC_DIR` (`web`),
`SITE_URL` (used in notification text).

## Deploy

### 1. Any small box (Pi, $5 VPS) — recommended, keeps SQLite

```bash
cargo build --release                     # static-ish binary
scp target/release/referral-ring web/ box:/opt/referral-ring/
sudo cp deploy/referral-ring.service /etc/systemd/system/
sudo systemctl enable --now referral-ring
```

Then put HTTPS in front with `deploy/Caddyfile` (two lines, automatic cert).
The database is one file: **back it up by copying it.**

### 2. Docker

```bash
docker build -f deploy/Dockerfile -t referral-ring .
docker run -d -p 8080:8080 -v ring-data:/data referral-ring
```

### 3. Frontend on GitHub Pages

Push to `main`; `.github/workflows/pages.yml` publishes `web/` to Pages.
Set `apiBase` in `web/config.js` to your API origin:

```js
window.RING_CONFIG = { apiBase: "https://ring.example.com" };
```

Leave it `""` if the Rust binary serves the UI itself (single origin, no CORS).

> A Pages-only deployment cannot write. The UI will say so plainly rather than
> inventing codes — a fake code that someone takes to Cape would be worse than
> an honest "offline".

---

## Contacting people

The original problem: "how does the page contact users to verify registration?"

Email is a liability (deliverability, phishing, an API key to leak) and Keet
can't be assumed. So the design uses two channels that need no account:

1. **A private management URL** — `/#/m/<32 chars>`. It is the login. Whoever
   holds it is the owner; it is never emailed and cannot be reset.
2. **ntfy push** (optional, one click to set up): install the free ntfy app,
   subscribe to a generated topic, get a push when someone uses your code.
   No signup, no key, no server-side mail credentials. Runs through
   `ntfy.sh` by default; point `NTFY_BASE` at a self-hosted instance if you'd
   rather not depend on it.

The full chain costs nothing and leaks nothing.

## Abuse & safety

- Rate limits per IP: 60 draws/h, 15 claims/h, 15 submissions/h.
- Pending claims expire after 7 days so nobody can squat a code forever.
- Rejecting a claim costs the referrer nothing and returns the code to the pool.
- Contacts are visible **only** to the owner of the code that was used.
- Codes are stored case-insensitively and normalised, so `k7q2 m4rb` and
  `K7Q2-M4RB` are the same code.
- What the site cannot do: verify anything with Cape. The referrer's own eyeballs
  on their own bill is the only real proof, which is why the confirmation step is
  the referrer's and is logged.

## API

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/pool` | counts per bin |
| POST | `/api/draw` | deal a code (reserves it for 72h) |
| POST | `/api/claims` | newcomer registers the code they used |
| GET | `/api/claims/{token}` | claim status for the newcomer |
| POST | `/api/claims/{token}/submit` | hand your own code back (**gated**) |
| GET | `/api/manage/{token}` | owner dashboard |
| POST | `/api/manage/{token}/confirm` | "the credit landed" |
| POST | `/api/manage/{token}/reject` | "it didn't" |
| POST | `/api/manage/{token}/lines` | add another line |
| POST | `/api/manage/{token}/notify` | set the ntfy topic |

## Layout

```
src/domain.rs   pure logic: weights, tokens, state machine   (no I/O)
src/db.rs       SQLite, transactions, SQL-enforced invariants
src/api.rs      axum handlers, validation, rate limits, ntfy
src/main.rs     server + `add-code` / `stats` CLI
web/            static UI, no build step, no dependencies
deploy/         Dockerfile, systemd unit, Caddyfile
```

MIT.
