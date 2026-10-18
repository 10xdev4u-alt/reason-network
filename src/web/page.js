const state = { sessions: [], selected: null, who: null, held: [], receipts: [], timer: null, es: null };

const $ = (id) => document.getElementById(id);

// The only way this page puts something in the document is a node built by hand, so a message body can never
// be read as markup, whatever it contains.
function show(box, text) {
  if (text === undefined) { box.replaceChildren(); return; }
  const d = document.createElement('div');
  d.className = 'empty';
  d.textContent = text;
  box.replaceChildren(d);
}

function toast(msg, ms = 2500) {
  const t = $('toast');
  t.textContent = msg;
  t.classList.remove('hidden');
  clearTimeout(t._t);
  t._t = setTimeout(() => t.classList.add('hidden'), ms);
}

async function fetchJSON(url, opts) {
  const r = await fetch(url, opts);
  if (!r.ok) {
    let m = r.status + ' ' + r.statusText;
    try { const j = await r.json(); if (j.error) m = j.error; } catch (_) {}
    throw new Error(m);
  }
  return r.json();
}

function tierClass(t) { return 'tier-' + (t || 'none').toLowerCase(); }
function statusClass(s) {
  if (s === 'busy') return 'status-busy';
  if (s === 'idle') return 'status-idle';
  if (s === 'offline') return 'status-offline';
  return 'status-idle';
}

async function refreshWho() {
  try {
    const d = await fetchJSON('/api/who');
    state.who = d;
    $('team-name').textContent = d.team || 'no team';
    $('me').textContent = d.person + ' on ' + d.machine;
    const unread = (d.sessions || []).reduce((a, s) => a + (s.unread || 0), 0);
    const badge = $('unread-badge');
    badge.textContent = unread;
    badge.title = unread + ' unread';
    badge.classList.toggle('hidden', unread === 0);

    const ms = $('machines');
    show(ms);
    (d.machines || []).forEach(m => {
      const chip = document.createElement('div');
      chip.className = 'machine-chip';
      const dot = document.createElement('span');
      dot.className = 'dot ' + (m.is_me ? 'on' : (m.online ? (m.link === 'relay' ? 'relay' : 'on') : 'off'));
      chip.appendChild(dot);
      const name = document.createElement('span');
      name.className = 'machine-name';
      name.textContent = m.person + ' · ' + m.machine;
      chip.appendChild(name);
      const sub = document.createElement('span');
      sub.className = 'machine-sub';
      sub.textContent = m.is_me ? 'you · this machine'
        : (m.online ? (m.link || 'connected') + (m.rtt_ms ? ' · ' + m.rtt_ms + 'ms' : '') : 'offline');
      chip.appendChild(sub);
      ms.appendChild(chip);
    });

    const filter = $('filter').value.toLowerCase();
    const list = (d.sessions || []).filter(s => {
      if (!filter) return true;
      return [s.address, s.agent, s.cwd, s.machine, s.repo, s.branch, s.doing].filter(Boolean).join(' ').toLowerCase().includes(filter);
    });
    const box = $('sessions');
    show(box);
    $('session-count').textContent = list.length + ' of ' + (d.sessions || []).length;
    if (list.length === 0) { show(box, 'no sessions'); return; }
    list.forEach(s => {
      const row = document.createElement('div');
      row.className = 'session-row' + (state.selected === s.address ? ' selected' : '');
      row.onclick = () => selectSession(s.address);
      const top = document.createElement('div');
      top.className = 'session-top';
      const sdot = document.createElement('span');
      sdot.className = 'dot ' + (s.status === 'offline' ? 'bad' : (s.status === 'busy' ? 'relay' : 'on'));
      top.appendChild(sdot);
      const name = document.createElement('span'); name.className = 'session-name'; name.textContent = s.name; name.title = s.name; top.appendChild(name);
      const agent = document.createElement('span'); agent.className = 'session-agent'; agent.textContent = s.agent; top.appendChild(agent);
      const st = document.createElement('span'); st.className = 'session-status ' + statusClass(s.status); st.textContent = s.status; top.appendChild(st);
      const tier = document.createElement('span'); tier.className = 'tier ' + tierClass(s.tier); tier.textContent = s.tier; top.appendChild(tier);
      if (s.unread > 0) { const u = document.createElement('span'); u.className = 'unread'; u.textContent = s.unread + ' unread'; top.appendChild(u); }
      if (s.is_me) { const m = document.createElement('span'); m.className = 'dim'; m.textContent = '← you'; top.appendChild(m); }
      row.appendChild(top);
      const meta = document.createElement('div'); meta.className = 'session-meta';
      meta.textContent = (s.repo ? s.repo + '@' + (s.branch || '') : s.cwd) + ' · ' + s.machine;
      row.appendChild(meta);
      if (s.doing) { const dd = document.createElement('div'); dd.className = 'session-doing'; dd.textContent = s.doing; row.appendChild(dd); }
      box.appendChild(row);
    });

    $('last-refresh').textContent = 'refreshed ' + new Date().toLocaleTimeString();
    state.sessions = d.sessions || [];
  } catch (e) {
    console.error('refresh failed', e);
    toast('refresh failed: ' + e.message, 4000);
  }
}

async function selectSession(addr) {
  state.selected = addr;
  $('inbox-target').textContent = '· ' + addr.split('/').pop();
  await refreshInbox();
  await refreshWho();
}

async function refreshInbox() {
  if (!state.selected) { show($('inbox'), 'select a session'); return; }
  try {
    const i = await fetchJSON('/api/inbox?address=' + encodeURIComponent(state.selected));
    const box = $('inbox');
    show(box);
    if (!i.messages || i.messages.length === 0) { show(box, 'no messages'); return; }
    i.messages.forEach(m => {
      const el = document.createElement('div');
      el.className = 'msg';
      const head = document.createElement('div'); head.className = 'msg-head';
      const left = document.createElement('span'); left.textContent = m.from;
      const right = document.createElement('span'); right.textContent = new Date(m.sent_at).toLocaleTimeString();
      head.appendChild(left); head.appendChild(right);
      el.appendChild(head);
      const body = document.createElement('div'); body.className = 'msg-body'; body.textContent = m.body;
      el.appendChild(body);
      box.appendChild(el);
    });
    box.scrollTop = box.scrollHeight;
  } catch (e) { show($('inbox'), e.message); }
}

async function refreshHeld() {
  try {
    const msgs = await fetchJSON('/api/held');
    state.held = msgs;
    const box = $('held');
    show(box);
    if (!msgs || msgs.length === 0) { show(box, 'nothing held'); return; }
    msgs.forEach(m => {
      const el = document.createElement('div');
      el.className = 'held-item';
      const h = document.createElement('div'); h.className = 'msg-head';
      const f = document.createElement('span'); f.textContent = m.from; h.appendChild(f);
      const i = document.createElement('span'); i.textContent = m.id; h.appendChild(i);
      el.appendChild(h);
      const b = document.createElement('div'); b.className = 'msg-body'; b.textContent = m.body || ''; el.appendChild(b);
      const actions = document.createElement('div'); actions.className = 'held-actions';
      const approve = document.createElement('button'); approve.className = 'btn primary'; approve.textContent = 'approve';
      approve.onclick = async () => { await fetchJSON('/api/settle', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({ids:[m.id], release:true})}); toast('approved'); refreshHeld(); };
      const reject = document.createElement('button'); reject.className = 'btn'; reject.textContent = 'reject';
      reject.onclick = async () => { await fetchJSON('/api/settle', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({ids:[m.id], release:false})}); toast('rejected'); refreshHeld(); };
      actions.appendChild(approve); actions.appendChild(reject);
      el.appendChild(actions);
      box.appendChild(el);
    });
    renderMailStats();
  } catch (e) { console.error('held failed', e); }
}

// What the left column counts comes from the same rows the board renders, so a number cannot drift from a list.
function statRow(box, label, value, dot) {
  const r = document.createElement('div'); r.className = 'stat';
  const d = document.createElement('span'); d.className = 'dot ' + (dot || 'off'); r.appendChild(d);
  const l = document.createElement('span'); l.textContent = label; r.appendChild(l);
  const v = document.createElement('b'); v.textContent = value; r.appendChild(v);
  box.appendChild(r);
}

function renderBoardStats() {
  const box = $('board-stats');
  show(box);
  const tasks = state.tasks || [];
  if (!tasks.length) { show(box, 'nothing open'); return; }
  ['open', 'claimed', 'blocked'].forEach(s => {
    statRow(box, s, String(tasks.filter(t => t.state === s).length), s);
  });
  // shortcut: one tick per task, capped at 60; past that the bar reads as "full", which the count still tells.
  const bar = document.createElement('div'); bar.className = 'bar';
  tasks.slice(0, 60).forEach(t => { const seg = document.createElement('i'); seg.className = t.state; bar.appendChild(seg); });
  box.appendChild(bar);
}

// The ring, rendered: what happened to the mail this machine sent. The daemon keeps it; this is one view of it.
async function refreshReceipts() {
  try {
    state.receipts = await fetchJSON('/api/receipts');
  } catch (e) { show($('receipts'), 'audit: ' + e.message); return; }
  const box = $('receipts');
  show(box);
  const rs = (state.receipts || []).slice(0, 30);
  $('audit-count').textContent = rs.length ? rs.length + ' recent' : '';
  renderMailStats();
  if (!rs.length) { show(box, 'no deliveries yet'); return; }
  rs.forEach(r => {
    const el = document.createElement('div'); el.className = 'receipt';
    const d = document.createElement('span');
    d.className = 'dot ' + (r.state === 'delivered' ? 'on' : (r.state === 'refused' ? 'bad' : 'relay'));
    el.appendChild(d);
    const s = document.createElement('span'); s.className = 'receipt-state'; s.textContent = r.state; el.appendChild(s);
    const to = document.createElement('span'); to.className = 'receipt-to'; to.textContent = r.to; el.appendChild(to);
    const t = document.createElement('span'); t.className = 't'; t.textContent = new Date(r.at_ms).toLocaleTimeString(); el.appendChild(t);
    box.appendChild(el);
  });
}

function renderMailStats() {
  const box = $('mail-stats');
  show(box);
  const held = (state.held || []).length;
  statRow(box, 'held', String(held), held ? 'relay' : 'off');
  ['delivered', 'refused', 'abandoned'].forEach(k => {
    const n = (state.receipts || []).filter(r => r.state === k).length;
    statRow(box, k, String(n), k === 'delivered' ? 'on' : (k === 'refused' ? 'bad' : 'relay'));
  });
}

async function doSend() {
  const to = $('send-to').value.trim();
  const body = $('send-body').value.trim();
  const notify = $('send-notify').checked;
  if (!to || !body) { toast('to and body required'); return; }
  try {
    const r = await fetchJSON('/api/send', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({from: '', to, body, notify})});
    const summary = (r.recipients || []).map(x => x.address + ': ' + x.state).join(', ');
    $('send-result').textContent = summary + (r.ids ? ' · ' + r.ids.join(', ') : '');
    $('send-body').value = '';
    toast('sent → ' + summary);
    refreshWho();
  } catch (e) { $('send-result').textContent = 'error: ' + e.message; toast('send failed: ' + e.message, 4000); }
}

async function doReply() {
  const body = $('reply-body').value.trim();
  if (!state.selected || !body) { toast('select a session and type a reply'); return; }
  try {
    const i = await fetchJSON('/api/inbox?address=' + encodeURIComponent(state.selected));
    const last = i.messages && i.messages.length ? i.messages[i.messages.length-1] : null;
    const target = last ? last.from : state.selected;
    const r = await fetchJSON('/api/send', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({from: '', to: target, body, notify: false})});
    $('reply-body').value = '';
    toast('reply sent → ' + (r.recipients || []).map(x => x.address).join(', '));
    refreshInbox();
  } catch (e) { toast('reply failed: ' + e.message, 4000); }
}

function startPolling() {
  if (state.timer) clearInterval(state.timer);
  state.timer = setInterval(() => {
    if (document.visibilityState === 'hidden') return;
    refreshWho(); refreshHeld(); refreshBoard(); refreshReceipts(); if (state.selected) refreshInbox();
  }, 5000);
}

function startSSE() {
  if (typeof EventSource === 'undefined') return;
  try {
    state.es = new EventSource('/api/events');
    state.es.onopen = () => { $('live').className = 'live on'; };
    state.es.onerror = () => { $('live').className = 'live poll'; };
    state.es.onmessage = (e) => {
      try { const d = JSON.parse(e.data); if (d.type === 'changed') { refreshWho(); refreshHeld(); refreshBoard(); refreshReceipts(); if (state.selected) refreshInbox(); } } catch (_) {}
    };
  } catch (e) { console.error('sse failed', e); }
}

document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible') { refreshWho(); refreshHeld(); if (state.selected) refreshInbox(); }
});

$('filter').addEventListener('input', refreshWho);
// `/` jumps to the filter, the way a status board should: no mouse to look something up.
document.addEventListener('keydown', (e) => {
  if (e.key !== '/' || e.metaKey || e.ctrlKey || e.altKey) return;
  const t = document.activeElement && document.activeElement.tagName;
  if (t === 'INPUT' || t === 'TEXTAREA') return;
  e.preventDefault();
  $('filter').focus();
});
$('send-btn').addEventListener('click', doSend);
$('reply-send').addEventListener('click', doReply);
$('theme-toggle').addEventListener('click', () => {
  const html = document.documentElement;
  const next = html.getAttribute('data-theme') === 'dark' ? 'light' : 'dark';
  html.setAttribute('data-theme', next);
  document.querySelector('meta[name="color-scheme"]').setAttribute('content', next);
});

// -- the board: the queue the daemon owns, rendered. A claim here is the person taking a task; an agent takes
// one over IPC (`reason network task claim`) with no browser involved, which is the point of the queue living in
// the daemon. `absent` on a claim is not `offline`: presence is a snapshot, and its owner may be mid-turn.
function age(ms) {
  if (ms === undefined) return '';
  if (ms < 60000) return 'just now';
  const m = Math.floor(ms / 60000);
  if (m < 60) return m + 'm';
  const h = Math.floor(m / 60);
  return h < 24 ? h + 'h' : Math.floor(h / 24) + 'd';
}

function ownerLine(t) {
  if (!t.owner) return 'unclaimed';
  const s = t.owner_status || 'absent';
  return t.owner + ' · ' + (s === 'absent' ? 'absent (may be mid-turn)' : s) + (t.owner_doing ? ' · ' + t.owner_doing : '');
}

async function refreshBoard() {
  try {
    const d = await fetchJSON('/api/tasks');
    state.tasks = d.tasks || [];
    const open = state.tasks.filter(t => t.state !== 'done').length;
    $('board-count').textContent = '· ' + open + ' open';
    renderBoardStats();
    const box = $('board');
    show(box);
    if (!state.tasks.length) { show(box, 'nothing on the board'); return; }
    state.tasks.forEach(t => {
      const row = document.createElement('div');
      row.className = 'task-row' + (state.selectedTask === t.id ? ' selected' : '');
      row.onclick = () => selectTask(t.id);
      const top = document.createElement('div'); top.className = 'task-top';
      const dot = document.createElement('span'); dot.className = 'dot ' + t.state; top.appendChild(dot);
      const ti = document.createElement('span'); ti.className = 'task-title'; ti.textContent = t.title; ti.title = t.title; top.appendChild(ti);
      const a = document.createElement('span'); a.className = 'task-age'; a.textContent = age(t.age_ms); top.appendChild(a);
      row.appendChild(top);
      const meta = document.createElement('div'); meta.className = 'task-meta';
      const st = document.createElement('span'); st.className = 'task-state task-' + t.state; st.textContent = t.state;
      meta.appendChild(st);
      meta.appendChild(document.createTextNode(' ' + ownerLine(t)));
      row.appendChild(meta);
      const bd = document.createElement('div'); bd.className = 'task-boundary'; bd.textContent = 'done: ' + t.boundary; row.appendChild(bd);
      box.appendChild(row);
    });
  } catch (e) { show($('board'), 'board: ' + e.message); }
}

async function selectTask(id) {
  state.selectedTask = id;
  try {
    state.taskDetail = await fetchJSON('/api/tasks/show?id=' + encodeURIComponent(id));
  } catch (e) { state.taskDetail = null; toast(e.message); }
  renderDetail();
  refreshBoard();
}

function renderDetail() {
  const box = $('task-detail');
  const d = state.taskDetail;
  show(box);
  if (!d) { show(box, 'select a task'); return; }
  const t = d.task;
  const head = document.createElement('div'); head.className = 'task-top';
  const ti = document.createElement('span'); ti.className = 'task-title'; ti.textContent = t.title; ti.title = t.title; head.appendChild(ti);
  const st = document.createElement('span'); st.className = 'task-state task-' + t.state; st.textContent = t.state; head.appendChild(st);
  box.appendChild(head);
  const bd = document.createElement('div'); bd.className = 'task-boundary'; bd.textContent = 'done: ' + t.boundary; box.appendChild(bd);
  const own = document.createElement('div'); own.className = 'task-meta'; own.textContent = ownerLine(t); box.appendChild(own);
  if (t.blocked_reason) { const b = document.createElement('div'); b.className = 'task-reason'; b.textContent = 'blocked: ' + t.blocked_reason; box.appendChild(b); }
  const hist = document.createElement('div'); hist.className = 'task-history';
  (d.events || []).forEach(e => {
    const line = document.createElement('div'); line.className = 'task-event';
    line.textContent = new Date(e.at_ms).toLocaleTimeString() + ' · ' + e.kind + ' · ' + e.by + (e.text ? ' · ' + e.text : '');
    hist.appendChild(line);
  });
  box.appendChild(hist);
}

function wireActions() {
  const box = $('task-actions');
  [['take it', 'claim', false], ['release', 'release', false], ['note', 'note', true], ['block', 'block', true], ['close with evidence', 'close', true]].forEach(([label, path, needs]) => {
    const btn = document.createElement('button');
    btn.className = 'btn' + (path === 'claim' ? ' primary' : '');
    btn.textContent = label;
    btn.onclick = () => taskAction(path, needs);
    box.appendChild(btn);
  });
}

async function taskAction(path, needsText) {
  const text = $('task-text').value.trim();
  if (needsText && !text) { toast(path === 'close' ? 'closing needs evidence' : 'say why, in the box below'); return; }
  const body = { id: state.selectedTask, text, reason: text, evidence: text };
  try {
    const r = await fetchJSON('/api/tasks/' + path, {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(body)});
    $('task-text').value = '';
    toast(path === 'claim' ? (r.ok ? 'yours' : 'held by ' + r.owner) : 'ok');
    await selectTask(state.selectedTask);
  } catch (e) { toast(e.message, 4000); }
}

async function doAddTask() {
  const title = $('task-title').value.trim();
  const boundary = $('task-boundary').value.trim();
  if (!title || !boundary) { toast('a task needs a title and what done means'); return; }
  try {
    await fetchJSON('/api/tasks', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({title, boundary})});
    $('task-title').value = ''; $('task-boundary').value = '';
    toast('added');
    refreshBoard();
  } catch (e) { toast(e.message, 4000); }
}

$('task-add').addEventListener('click', doAddTask);
wireActions();

$('live').className = 'live poll';
refreshWho();
refreshHeld();
renderDetail();
refreshBoard();
refreshReceipts();
startPolling();
startSSE();
