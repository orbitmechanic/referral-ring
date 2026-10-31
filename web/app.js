/* Referral Ring — no framework, no build step, no dependencies.
   Hash routes:  #/            home + machine
                 #/m/<token>   owner dashboard
                 #/c/<token>   claimant status / code hand-back          */

(() => {
  'use strict';

  const API = (window.RING_CONFIG?.apiBase || '').replace(/\/+$/, '');
  const $ = (sel, root = document) => root.querySelector(sel);
  const el = (tag, props = {}, ...kids) => {
    const n = Object.assign(document.createElement(tag), props);
    for (const k of kids) n.append(k);
    return n;
  };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

  // ---------------------------------------------------------------- storage
  const store = {
    get(k, d) { try { return JSON.parse(localStorage.getItem('rr.' + k)) ?? d; } catch { return d; } },
    set(k, v) { try { localStorage.setItem('rr.' + k, JSON.stringify(v)); } catch {} },
  };
  const myClaims = () => store.get('claims', []);
  const rememberClaim = (t) => { const a = myClaims(); if (!a.includes(t)) { a.unshift(t); store.set('claims', a.slice(0, 6)); } };

  // ---------------------------------------------------------------- http
  async function api(path, opts = {}) {
    const res = await fetch(API + path, {
      headers: { 'content-type': 'application/json' },
      ...opts,
      body: opts.body ? JSON.stringify(opts.body) : undefined,
    });
    let data = null;
    try { data = await res.json(); } catch {}
    if (!res.ok) {
      const err = new Error(data?.message || `request failed (${res.status})`);
      err.status = res.status;
      err.code = data?.error;
      throw err;
    }
    return data;
  }

  // ---------------------------------------------------------------- toast
  let toastTimer;
  function toast(msg, kind = '') {
    const t = $('#toast');
    t.textContent = msg;
    t.className = 'toast show ' + kind;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { t.className = 'toast ' + kind; }, 3200);
  }

  async function copy(text, label = 'Copied') {
    try {
      await navigator.clipboard.writeText(text);
      toast(label, 'good');
    } catch {
      const ta = el('textarea', { value: text });
      document.body.append(ta);
      ta.select();
      document.execCommand?.('copy');
      ta.remove();
      toast(label + ' (copied the old way)', 'good');
    }
  }

  const baseUrl = () => location.origin + location.pathname;
  const manageUrl = (tok) => `${baseUrl()}#/m/${tok}`;
  const claimUrl = (tok) => `${baseUrl()}#/c/${tok}`;

  // ---------------------------------------------------------------- router
  const views = { home: $('#view-home'), manage: $('#view-manage'), claim: $('#view-claim') };

  function show(which) {
    for (const [k, node] of Object.entries(views)) node.hidden = k !== which;
    window.scrollTo({ top: 0, behavior: 'instant' });
  }

  async function route() {
    const hash = location.hash || '#/';
    let m;
    if ((m = hash.match(/^#\/m\/([A-Z0-9]+)$/i))) {
      show('manage');
      await renderManage(m[1]);
    } else if ((m = hash.match(/^#\/c\/([A-Z0-9]+)$/i))) {
      show('claim');
      await renderClaim(m[1]);
    } else {
      show('home');
      loadPool();
    }
  }

  // ---------------------------------------------------------------- machine
  const reel = $('#reel');
  const lever = $('#lever');
  const CHARS = 'ABCDEFGHJKMNPQRSTVWXYZ23456789';
  const randChar = () => CHARS[Math.floor(Math.random() * CHARS.length)];

  function buildReel(code, spinning) {
    reel.innerHTML = '';
    reel.style.fontSize = code.length > 12 ? 'clamp(.95rem, 3vw, 1.5rem)' : '';
    const cells = [];
    const groups = code.match(/.{1,4}/g) || [''];
    groups.forEach((g, gi) => {
      for (const ch of g) {
        const c = el('span', { className: 'cell' });
        c.textContent = spinning ? randChar() : ch;
        reel.append(c);
        cells.push(c);
      }
      if (gi < groups.length - 1) {
        const d = el('span', { className: 'cell spacer' });
        d.textContent = '';
        reel.append(d);
      }
    });
    return cells;
  }

  function idleReel() {
    reel.classList.remove('spinning', 'landed');
    reel.innerHTML = '';
    for (let i = 0; i < 8; i++) {
      const c = el('span', { className: 'cell' });
      c.textContent = '·';
      c.style.color = '#2a2a30';
      reel.append(c);
    }
  }

  /** Scramble, then land left-to-right. The draw itself already happened on the server. */
  async function spinTo(code) {
    reel.classList.add('spinning');
    reel.classList.remove('landed');
    let cells = buildReel(code, true);
    const scramble = setInterval(() => {
      for (const c of cells) if (!c.dataset.lock) c.textContent = randChar();
    }, 55);

    await sleep(420);
    // real length is known now; rebuild so the frame is exactly the code
    clearInterval(scramble);
    cells = buildReel(code, true);
    const scramble2 = setInterval(() => {
      for (const c of cells) if (!c.dataset.lock) c.textContent = randChar();
    }, 55);

    for (let i = 0; i < cells.length; i++) {
      await sleep(i === 0 ? 220 : 85);
      const c = cells[i];
      c.dataset.lock = '1';
      c.textContent = code.replace(/-/g, '')[i] ?? '';      c.classList.add('drop');
      setTimeout(() => c.classList.remove('drop'), 400);
    }
    clearInterval(scramble2);
    reel.classList.remove('spinning');
    reel.classList.add('landed');
  }

  function setPips(n, total = 4) {
    const box = $('#pips');
    box.innerHTML = '';
    for (let i = 0; i < total; i++) {
      const b = el('b');
      if (i < n) b.classList.add('on');
      box.append(b);
    }
  }

  async function loadPool() {
    const lamp = $('#lamp');
    const light = $('#poolLight');
    try {
      const p = await api('/api/pool');
      lamp.className = 'lamp ' + (p.total > 0 ? 'ok' : 'warn');
      light.textContent = p.total > 0 ? `${p.total} code${p.total === 1 ? '' : 's'} in the ring` : 'ring is empty — be the first';
      renderBars(p.bins, p.total);
      return p;
    } catch (e) {
      lamp.className = 'lamp dead';
      light.textContent = 'dispenser offline';
      renderBars(null, 0);
      return null;
    }
  }

  function renderBars(bins, total) {
    const host = $('#bars');
    host.innerHTML = '';
    const max = Math.max(4, ...(bins || []).map((b) => b.count));
    for (const need of [4, 3, 2, 1]) {
      const b = (bins || []).find((x) => x.need === need) || { need, count: 0 };
      const pct = Math.round((b.count / max) * 100);
      host.append(
        el('div', { className: 'bar-row' },
          el('div', { className: 'bar-label', innerHTML: `<b>${need}</b> needed${need === 4 ? ' <span title="free line">★</span>' : ''}` }),
          el('div', { className: 'bar-track' }, el('div', { className: 'bar-fill', style: `width:${pct}%` })),
          el('div', { className: 'bar-count', textContent: b.count }),
        ),
      );
    }
    $('#poolLegend').textContent = bins
      ? `${total} line${total === 1 ? '' : 's'} dealable right now. Codes held by someone else, or cooling down, aren't counted.`
      : 'The dispenser could not reach the ring. If you are running this from GitHub Pages, set apiBase in config.js.';
  }

  // ---------------------------------------------------------------- draw
  let lastDraw = null;

  async function pull() {
    if (lever.disabled) return;
    lever.disabled = true;
    $('#tray').hidden = true;
    setPips(0);
    $('#machineHint').textContent = 'pulling…';
    const started = spinTo('???????');

    let res;
    try {
      res = await api('/api/draw', { method: 'POST', body: { exclude: store.get('recent', []) } });
    } catch (e) {
      await started;
      reel.classList.remove('spinning');
      lever.disabled = false;
      $('#machineHint').textContent = 'nothing came out. try again.';
      if (e.code === 'pool_empty') {
        openTray('The ring is empty', `<p>Every code is currently signed up, in flight, or cooling down. That is a good sign, not a bad one — come back in a few minutes, or add your own line so the next person isn't stuck.</p>`);
      } else if (e.status === 429) {
        toast('Slow down a moment — the machine needs a breather.', 'bad');
      } else {
        openTray('The dispenser is offline', `<p>${esc(e.message)}</p><p class="form-note">If this site is on GitHub Pages, point <code>apiBase</code> in <code>config.js</code> at the API server.</p>`);
      }
      return;
    }

    await started;
    await spinTo(res.code);
    lastDraw = res;

    const recent = store.get('recent', []);
    recent.unshift(res.code);
    store.set('recent', recent.slice(0, 6));

    setPips(res.remaining);
    $('#machineHint').textContent = res.remaining === 4
      ? 'A free line in the making — four referrals and Cape stops charging.'
      : `This line still needs ${res.remaining} referral${res.remaining === 1 ? '' : 's'} — worth $20 a month to them.`;
    lever.disabled = false;
    renderDraw(res);
  }

  function openTray(title, html) {
    $('#trayTitle').textContent = title;
    $('#trayBody').innerHTML = html;
    $('#tray').hidden = false;
  }

  function renderDraw(d) {
    const hours = Math.max(1, Math.round((d.expires_at * 1000 - Date.now()) / 3600000));
    openTray("This one's yours", `
      <div class="codeBox">
        <code>${esc(d.code)}</code>
        <button class="btn small quiet" data-copy="${esc(d.code)}">Copy</button>
      </div>
      <p>Held for you for ${hours} hours. Write it down — you'll need it in a moment, and coming back to this page without it means losing your place.</p>
      <ol>
        <li>Go to <a href="https://www.cape.co" target="_blank" rel="noopener">cape.co</a> and start service.</li>
        <li>Enter <b>${esc(d.code)}</b> as the referral code. You both get $20 off, every month, while you both stay subscribed.</li>
        <li>Come back here and register the claim below.</li>
      </ol>
      <div class="tray-actions">
        <button class="btn primary" id="toClaimForm">I've signed up — register my claim</button>
        <button class="btn quiet" id="pullAgain">Pull another</button>
      </div>
    `);
    $('#toClaimForm').onclick = () => renderClaimForm(d);
    $('#pullAgain').onclick = pull;
    bindCopy();
  }

  function renderClaimForm(d) {
    openTray('Register your claim', `
      <p>This tells the owner of <b>${esc(d.code)}</b> that you used their code, so they can check their bill. They have to confirm the credit actually appeared before you can add your own code — that's the lock that keeps the ring honest.</p>
      <form class="stack" id="claimForm">
        <label class="field">What should they call you?
          <input type="text" id="cLabel" maxlength="40" placeholder="optional — first name or handle">
        </label>
        <label class="field">How can they reach you?
          <input type="text" id="cContact" required maxlength="120" placeholder="email, phone, or Keet handle">
        </label>
        <p class="form-note">Only the person whose code you used sees this. It is never listed publicly.</p>
        <button class="btn primary" type="submit">Send the claim</button>
      </form>
      <p class="err" id="claimErr" hidden></p>
    `);
    $('#claimForm').onsubmit = async (ev) => {
      ev.preventDefault();
      const btn = ev.target.querySelector('button[type=submit]');
      btn.disabled = true;
      try {
        const r = await api('/api/claims', {
          method: 'POST',
          body: { code: d.code, label: $('#cLabel').value, contact: $('#cContact').value },
        });
        rememberClaim(r.token);
        renderClaimMade(d, r.token);
      } catch (e) {
        const p = $('#claimErr');
        p.textContent = e.message;
        p.hidden = false;
        btn.disabled = false;
      }
    };
  }

  function renderClaimMade(d, token) {
    const url = claimUrl(token);
    openTray('Claim sent', `
      <p>They've been pinged if they set up phone alerts. Now it's their move: they check their Cape bill, and if the $20 credit landed they confirm it here.</p>
      <p><b>Bookmark this link.</b> It's how you check the status — and the only way to add your own code once they confirm.</p>
      <div class="keybox">${esc(url)}</div>
      <div class="tray-actions">
        <button class="btn primary" data-copy="${esc(url)}">Copy my claim link</button>
        <a class="btn quiet" href="#/c/${encodeURIComponent(token)}">Check status</a>
      </div>
      <p class="form-note">No email, no password. If you lose this link you can still contact the owner, but the site can't recover it for you.</p>
    `);
    bindCopy();
  }

  function bindCopy() {
    document.querySelectorAll('[data-copy]').forEach((b) => {
      b.onclick = () => copy(b.dataset.copy, 'Copied');
    });
  }

  // ---------------------------------------------------------------- manage
  async function renderManage(token) {
    const host = $('#manageBody');
    host.innerHTML = '<p class="muted">loading your lines…</p>';
    store.set('account', token);
    let data;
    try {
      data = await api(`/api/manage/${encodeURIComponent(token)}`);
    } catch (e) {
      host.innerHTML = `<h1>That link didn't open</h1><p class="muted">${esc(e.message)}</p>
        <p class="form-note">Management keys are 32 characters and cannot be reset. Check for a missing character.</p>`;
      return;
    }

    const pending = data.lines.flatMap((l) => l.claims.filter((c) => c.state === 'pending'));
    const frag = document.createDocumentFragment();
    frag.append(el('h1', { textContent: data.label ? `${data.label}'s lines` : 'Your lines' }));
    frag.append(el('p', { className: 'muted', textContent: pending.length
      ? `${pending.length} claim${pending.length === 1 ? '' : 's'} waiting on you to check your Cape bill.`
      : 'Nothing waiting right now. Your codes are in the pool.' }));

    for (const line of data.lines) frag.append(lineCard(line, token, pending.length));

    frag.append(addLineCard(token));
    frag.append(notifyCard(token, data.notify_topic));

    const share = el('div', { className: 'line-card' });
    share.innerHTML = `
      <div class="line-top"><div>
        <div class="line-code" style="font-size:1rem">Your management link</div>
        <div class="line-meta">Bookmark it. Anyone holding it can act as you, and it cannot be reset.</div>
      </div>
      <button class="btn small quiet" data-copy="${esc(manageUrl(token))}">Copy</button></div>
      <div class="keybox">${esc(manageUrl(token))}</div>`;
    frag.append(share);

    host.replaceChildren(frag);
    bindCopy();
    wireManage(token);
  }

  function pipsHtml(remaining) {
    let s = '<span class="pips">';
    for (let i = 0; i < 4; i++) s += `<b class="${i < remaining ? 'on' : ''}"></b>`;
    return s + '</span>';
  }

  function lineCard(line, token, pendingCount) {
    const card = el('div', { className: 'line-card' });
    const open = line.claims.filter((c) => c.state === 'pending' || c.state === 'confirmed');
    const closed = line.claims.filter((c) => !open.includes(c));

    card.innerHTML = `
      <div class="line-top">
        <div>
          <div class="line-code">${esc(line.code)}</div>
          <div class="line-meta">${line.label ? esc(line.label) + ' · ' : ''}${
            line.status === 'exhausted' ? 'fully rewarded — retired' :
            `${line.remaining} of ${line.need} referral${line.need === 1 ? '' : 's'} still needed`
          }${line.served ? ` · dealt ${line.served}×` : ''}${
            line.held ? ' · <span style="color:var(--amber)">held by someone for 72h</span>' : ''
          }</div>
        </div>
        <div>${pipsHtml(line.status === 'exhausted' ? 0 : line.remaining)}</div>
      </div>
      ${line.sponsored.length ? `<div class="line-meta" style="margin-top:.5rem">brought in: ${line.sponsored.map(esc).join(', ')}</div>` : ''}
      <div data-claims="${line.id}"></div>`;

    const box = card.querySelector(`[data-claims="${line.id}"]`);
    for (const c of open) {
      const row = el('div', { className: 'claim' });
      row.innerHTML = `
        <div class="claim-info">
          <span class="badge ${c.state}">${c.state}</span>
          <b style="margin-left:.4rem">${esc(c.label || 'someone')}</b> used your code
          <small>${esc(c.contact)} · filed ${when(c.created_at)}
            ${c.state === 'confirmed' ? '· you confirmed the credit; waiting on their code' : '· they are waiting on you'}</small>
        </div>
        <div class="claim-actions">
          ${c.state === 'pending' ? `
            <button class="btn small primary" data-confirm="${c.id}">Credit landed</button>
            <button class="btn small danger" data-reject="${c.id}">Not there</button>` :
            `<span class="badge confirmed">unlocked</span>`}
        </div>`;
      box.append(row);
    }
    for (const c of closed.slice(0, 3)) {
      const row = el('div', { className: 'claim' });
      row.innerHTML = `<div class="claim-info"><span class="badge ${c.state}">${c.state}</span>
        <b style="margin-left:.4rem">${esc(c.label || 'someone')}</b>
        <small>${esc(c.contact)} · ${when(c.created_at)}${c.note ? ' · ' + esc(c.note) : ''}</small></div>`;
      box.append(row);
    }
    return card;
  }

  function addLineCard(token) {
    const card = el('div', { className: 'line-card' });
    card.innerHTML = `
      <div class="line-top"><div>
        <div class="line-code" style="font-size:1rem">Add another line</div>
        <div class="line-meta">Family lines, a second phone, a spouse's code — anything you own.</div>
      </div></div>
      <form class="stack" id="addLine" style="max-width:34rem">
        <div style="display:flex;gap:.6rem;flex-wrap:wrap">
          <label class="field" style="flex:1 1 12rem">Referral code
            <input type="text" id="lineCode" required maxlength="24" placeholder="K7Q2M4RB" spellcheck="false">
          </label>
          <label class="field" style="flex:1 1 12rem">Label
            <input type="text" id="lineLabel" maxlength="40" placeholder="e.g. wife's phone">
          </label>
        </div>
        <div class="field">Referrals still needed
          <div class="needs-picker" id="lineNeeds" role="group">
            ${[4, 3, 2, 1].map((n) => `<button type="button" data-n="${n}" aria-pressed="${n === 1}">${n}${n === 4 ? ' (free)' : ''}</button>`).join('')}
          </div>
        </div>
        <button class="btn primary" type="submit">Add to the ring</button>
      </form>`;
    card.querySelectorAll('#lineNeeds button').forEach((b) => {
      b.onclick = () => {
        card.querySelectorAll('#lineNeeds button').forEach((x) => x.setAttribute('aria-pressed', 'false'));
        b.setAttribute('aria-pressed', 'true');
      };
    });
    card.querySelector('#addLine').onsubmit = async (ev) => {
      ev.preventDefault();
      const need = Number(card.querySelector('#lineNeeds [aria-pressed=true]').dataset.n);
      try {
        await api(`/api/manage/${encodeURIComponent(token)}/lines`, {
          method: 'POST',
          body: { code: card.querySelector('#lineCode').value, need, label: card.querySelector('#lineLabel').value },
        });
        toast('Line added to the pool', 'good');
        renderManage(token);
      } catch (e) {
        toast(e.message, 'bad');
      }
    };
    return card;
  }

  function notifyCard(token, current) {
    const card = el('div', { className: 'line-card' });
    const topic = current || 'rr-' + Math.random().toString(36).slice(2, 10);
    card.innerHTML = `
      <div class="line-top"><div>
        <div class="line-code" style="font-size:1rem">Phone alerts</div>
        <div class="line-meta">Free, no account. Install <a href="https://ntfy.sh" target="_blank" rel="noopener">ntfy</a> on your phone and subscribe to this topic — you'll get a push the moment someone uses your code.</div>
      </div></div>
      <div class="keybox">${esc(topic)}</div>
      <div class="tray-actions">
        <button class="btn small quiet" id="nCopy">Copy topic</button>
        <button class="btn small quiet" id="nTest">Send a test</button>
        <button class="btn small primary" id="nSave">${current ? 'Saved ✓' : 'Use this topic'}</button>
      </div>
      <p class="form-note">Optional. Without it you just check this page when it's convenient.</p>`;
    card.querySelector('#nCopy').onclick = () => copy(topic, 'Topic copied');
    card.querySelector('#nTest').onclick = async () => {
      try {
        await fetch('https://ntfy.sh/' + encodeURIComponent(topic), {
          method: 'POST',
          headers: { Title: 'Referral Ring test', Tags: 'bell' },
          body: 'If you can read this, phone alerts work. Keep this topic private.',
        });
        toast('Test sent — check your phone', 'good');
      } catch {
        toast('Could not reach ntfy.sh', 'bad');
      }
    };
    card.querySelector('#nSave').onclick = async () => {
      try {
        await api(`/api/manage/${encodeURIComponent(token)}/notify`, { method: 'POST', body: { topic } });
        toast('Phone alerts saved', 'good');
        renderManage(token);
      } catch (e) {
        toast(e.message, 'bad');
      }
    };
    return card;
  }

  function wireManage(token) {
    document.querySelectorAll('[data-confirm]').forEach((b) => {
      b.onclick = async () => {
        if (!confirm('Confirm that the $20 credit from this person is on your Cape account? This unlocks their code hand-back.')) return;
        b.disabled = true;
        try {
          await api(`/api/manage/${encodeURIComponent(token)}/confirm`, { method: 'POST', body: { claim_id: Number(b.dataset.confirm) } });
          toast('Confirmed — they can add their code now', 'good');
          renderManage(token);
        } catch (e) {
          toast(e.message, 'bad');
          b.disabled = false;
        }
      };
    });
    document.querySelectorAll('[data-reject]').forEach((b) => {
      b.onclick = async () => {
        const note = prompt('No credit yet? A short note for them (optional):', 'Not showing on my bill yet');
        if (note === null) return;
        b.disabled = true;
        try {
          await api(`/api/manage/${encodeURIComponent(token)}/reject`, { method: 'POST', body: { claim_id: Number(b.dataset.reject), note } });
          toast('Claim closed — the code goes back in the pool when they retry', 'good');
          renderManage(token);
        } catch (e) {
          toast(e.message, 'bad');
          b.disabled = false;
        }
      };
    });
  }

  // ---------------------------------------------------------------- claim view
  let claimPoll;

  async function renderClaim(token) {
    clearTimeout(claimPoll);
    const host = $('#claimBody');
    host.innerHTML = '<p class="muted">checking…</p>';
    let d;
    try {
      d = await api(`/api/claims/${encodeURIComponent(token)}`);
    } catch (e) {
      host.innerHTML = `<h1>That link didn't open</h1><p class="muted">${esc(e.message)}</p>`;
      return;
    }

    if (d.state === 'pending') {
      host.innerHTML = `
        <span class="badge pending">pending</span>
        <h1 style="margin-top:.8rem">Waiting on ${esc(d.referrer_label || 'the code owner')}</h1>
        <div class="status-big">
          <div class="line-code">${esc(d.referrer_code)}</div>
          <p class="muted" style="font-size:.88rem;margin:.5rem 0 0">You used this code. They've been notified and have to check their Cape bill for the $20 credit before you can add your own code.</p>
          <p class="form-note" style="margin-top:.8rem">Filed ${when(d.created_at)}. This page refreshes itself. Keep the link.</p>
        </div>
        <p class="form-note">Nothing to do but wait — or nudge them if you know them. Claims expire after 7 days, after which the code returns to the pool and you can pull a fresh one.</p>`;
      claimPoll = setTimeout(() => renderClaim(token), 20000);
      return;
    }

    if (d.state === 'confirmed') {
      const picker = [4, 3, 2, 1].map((n) => `<button type="button" data-n="${n}" aria-pressed="${n === 1}">${n}${n === 4 ? ' (free)' : ''}</button>`).join('');
      host.innerHTML = `
        <span class="badge confirmed">confirmed</span>
        <h1 style="margin-top:.8rem">The credit landed. Your turn.</h1>
        <p class="lede" style="margin-top:.7rem">${esc(d.referrer_label || 'The owner')} vouched for your signup on ${esc(d.referrer_code)}.
          Now put your own code in the ring so the next stranger can start where you started.</p>
        <form class="stack" id="ownForm">
          <label class="field">Your Cape referral code
            <input type="text" id="ownCode" required maxlength="24" placeholder="from the Cape app" spellcheck="false">
          </label>
          <div class="field">How many referrals do you still need?
            <div class="needs-picker" id="ownNeeds" role="group">${picker}</div>
          </div>
          <label class="field">Label
            <input type="text" id="ownLabel" maxlength="40" placeholder="optional — e.g. my line">
          </label>
          <label class="field">How people reach you
            <input type="text" id="ownContact" required maxlength="120" placeholder="email, phone, or Keet handle">
          </label>
          <p class="form-note">Shown only to people who register a claim on your code.</p>
          <button class="btn primary" type="submit">Add my code to the ring</button>
        </form>
        <p class="err" id="ownErr" hidden></p>`;
      host.querySelectorAll('#ownNeeds button').forEach((b) => {
        b.onclick = () => {
          host.querySelectorAll('#ownNeeds button').forEach((x) => x.setAttribute('aria-pressed', 'false'));
          b.setAttribute('aria-pressed', 'true');
        };
      });
      host.querySelector('#ownForm').onsubmit = async (ev) => {
        ev.preventDefault();
        const btn = ev.target.querySelector('button[type=submit]');
        btn.disabled = true;
        const need = Number(host.querySelector('#ownNeeds [aria-pressed=true]').dataset.n);
        try {
          const r = await api(`/api/claims/${encodeURIComponent(token)}/submit`, {
            method: 'POST',
            body: {
              code: host.querySelector('#ownCode').value,
              need,
              label: host.querySelector('#ownLabel').value,
              contact: host.querySelector('#ownContact').value,
            },
          });
          store.set('account', r.account_token);
          renderClaimDone(r);
        } catch (e) {
          const p = $('#ownErr');
          p.textContent = e.message;
          p.hidden = false;
          btn.disabled = false;
        }
      };
      return;
    }

    if (d.state === 'completed') {
      const mine = store.get('account');
      host.innerHTML = `
        <span class="badge completed">complete</span>
        <h1 style="margin-top:.8rem">The ring turned.</h1>
        <div class="status-big">
          <p style="margin:0;font-size:.9rem;color:#bdb8b0">Your code is live in the pool:</p>
          <div class="line-code" style="font-size:1.6rem;margin:.4rem 0">${esc(d.my_code || '')}</div>
          <p class="form-note">${esc(d.referrer_label || 'The owner')}'s line dropped by one referral. Yours is now waiting for its first pull.</p>
        </div>
        ${mine ? `<a class="btn primary" href="#/m/${encodeURIComponent(mine)}">Open my lines</a>` : ''}`;
      return;
    }

    host.innerHTML = `
      <span class="badge ${esc(d.state)}">${esc(d.state)}</span>
      <h1 style="margin-top:.8rem">This claim is closed</h1>
      <p class="muted">${
        d.state === 'rejected'
          ? 'The code owner said the credit had not appeared on their bill. That usually means the signup did not go through with that code, or they need another billing cycle. Pull a fresh code and try again.'
          : 'It sat unanswered for seven days and the code went back into the pool.'
      }</p>
      <p><a href="#/" class="btn quiet">Back to the machine</a></p>`;
  }

  function renderClaimDone(r) {
    const url = manageUrl(r.account_token);
    $('#claimBody').innerHTML = `
      <span class="badge completed">in the ring</span>
      <h1 style="margin-top:.8rem">You're in.</h1>
      <p class="lede" style="margin-top:.7rem"><b>${esc(r.code)}</b> is now one of the codes being dealt. When someone pulls it and signs up, you'll be able to confirm the credit and your count drops by one.</p>
      <div class="status-big">
        <div class="line-code" style="font-size:1rem">Your management link</div>
        <p class="form-note" style="margin-top:.4rem">This is shown once. It is the only key to your lines, and it cannot be reset by anyone.</p>
        <div class="keybox">${esc(url)}</div>
      </div>
      <div class="tray-actions">
        <button class="btn primary" id="saveKey">Copy and open my lines</button>
      </div>`;
    $('#saveKey').onclick = () => {
      copy(url, 'Link copied — bookmark it');
      location.hash = `#/m/${encodeURIComponent(r.account_token)}`;
    };
  }

  // ---------------------------------------------------------------- misc
  function when(unix) {
    const s = Math.max(0, Math.floor(Date.now() / 1000 - unix));
    if (s < 60) return 'just now';
    if (s < 3600) return `${Math.floor(s / 60)} min ago`;
    if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
    return `${Math.floor(s / 86400)} d ago`;
  }

  // ---------------------------------------------------------------- boot
  lever.onclick = pull;
  $('#trayClose').onclick = () => { $('#tray').hidden = true; };
  function focusManage() {
    $('#manageToken').focus();
    $('#manageOpen').scrollIntoView({ block: 'center', behavior: 'smooth' });
  }

  $('#navManage').onclick = () => {
    const tok = store.get('account');
    if (tok) {
      location.hash = `#/m/${encodeURIComponent(tok)}`;
      return;
    }
    if (views.home.hidden) {
      location.hash = '#/';
      setTimeout(focusManage, 120);
    } else {
      focusManage();
    }
  };

  $('#manageOpen').onsubmit = (ev) => {
    ev.preventDefault();
    const raw = $('#manageToken').value.trim();
    const m = raw.match(/([A-Z0-9]{16,40})\s*$/i);
    if (!m) return toast('That does not look like a management key', 'bad');
    location.hash = `#/m/${m[1].toUpperCase()}`;
  };

  window.addEventListener('hashchange', route);

  idleReel();
  setPips(0);
  route();

  // A returning claimant gets a nudge instead of hunting for their link.
  const pendingTokens = myClaims();
  if (pendingTokens.length) {
    const a = el('a', { className: 'btn quiet', href: `#/c/${encodeURIComponent(pendingTokens[0])}`, textContent: 'Check my claim →' });
    $('.hero-actions').append(a);
  }
})();
