const state = { sessions: [], selected: null, who: null, held: [], timer: null, es: null };

const $ = (id) => document.getElementById(id);

async function fetchJSON(url, opts) {
  const r = await fetch(url, opts);
  if (!r.ok) throw new Error(r.status + ' ' + r.statusText);
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
    const unread = d.sessions.reduce((a, s) => a + (s.unread || 0), 0);
    $('unread-badge').textContent = unread + ' unread';
    $('unread-badge').style.display = unread > 0 ? '' : 'none';

    // machines strip
    const ms = $('machines');
    ms.innerHTML = '';
    (d.machines || []).forEach(m => {
      const chip = document.createElement('div');
      chip.className = 'machine-chip';
      const dot = document.createElement('span');
      dot.className = 'dot ' + (m.is_me ? 'on' : (m.online ? (m.link === 'relay' ? 'relay' : 'on') : 'off'));
      chip.appendChild(dot);
      const label = document.createElement('span');
      label.textContent = m.person + ' · ' + m.machine + (m.is_me ? ' (you)' : (m.online ? ' · ' + (m.link || 'connected') + (m.rtt_ms ? ' ' + m.rtt_ms + 'ms' : '') : ' · offline'));
      chip.appendChild(label);
      ms.appendChild(chip);
    });

    // sessions
    const filter = $('filter').value.toLowerCase();
    const list = (d.sessions || []).filter(s => {
      if (!filter) return true;
      return [s.address, s.agent, s.cwd, s.machine, s.repo, s.branch, s.doing].filter(Boolean).join(' ').toLowerCase().includes(filter);
    });
    const box = $('sessions');
    box.innerHTML = '';
    list.forEach(s => {
      const row = document.createElement('div');
      row.className = 'session-row' + (state.selected === s.address ? ' selected' : '');
      row.onclick = () => selectSession(s.address);
      const top = document.createElement('div');
      top.className = 'session-top';
      const name = document.createElement('span'); name.className = 'session-name'; name.textContent = s.name; top.appendChild(name);
      const agent = document.createElement('span'); agent.className = 'session-agent'; agent.textContent = s.agent; top.appendChild(agent);
      const st = document.createElement('span'); st.className = 'session-status ' + statusClass(s.status); st.textContent = s.status; top.appendChild(st);
      const tier = document.createElement('span'); tier.className = 'tier ' + tierClass(s.tier); tier.textContent = s.tier; top.appendChild(tier);
      if (s.unread > 0) { const u = document.createElement('span'); u.className = 'unread'; u.textContent = s.unread + ' unread'; top.appendChild(u); }
      if (s.is_me) { const m = document.createElement('span'); m.className = 'dim'; m.textContent = '← you'; top.appendChild(m); }
      row.appendChild(top);
      const meta = document.createElement('div'); meta.className = 'session-meta'; meta.textContent = (s.repo ? s.repo + '@' + (s.branch || '') : s.cwd) + ' · ' + s.machine; row.appendChild(meta);
      if (s.doing) { const dd = document.createElement('div'); dd.className = 'session-doing'; dd.textContent = s.doing; row.appendChild(dd); }
      box.appendChild(row);
    });

    $('last-refresh').textContent = 'refreshed ' + new Date().toLocaleTimeString();
    state.sessions = d.sessions || [];
  } catch (e) {
    console.error('refresh failed', e);
  }
}

async function selectSession(addr) {
  state.selected = addr;
  $('inbox-target').textContent = addr;
  await refreshInbox();
  await refreshWho();
}

async function refreshInbox() {
  if (!state.selected) return;
  try {
    const i = await fetchJSON('/api/inbox?key=' + encodeURIComponent(state.selected));
    const box = $('inbox');
    box.innerHTML = '';
    if (!i.messages || i.messages.length === 0) {
      box.innerHTML = '<div class="dim">no messages</div>';
      return;
    }
    i.messages.forEach(m => {
      const el = document.createElement('div');
      el.className = 'msg';
      const head = document.createElement('div'); head.className = 'msg-head';
      head.textContent = m.from + ' · ' + new Date(m.sent_at).toLocaleTimeString() + ' · ' + m.id;
      el.appendChild(head);
      const body = document.createElement('div'); body.className = 'msg-body'; body.textContent = m.body;
      el.appendChild(body);
      box.appendChild(el);
    });
  } catch (e) { console.error('inbox failed', e); }
}

async function refreshHeld() {
  try {
    const msgs = await fetchJSON('/api/held');
    state.held = msgs;
    const box = $('held');
    box.innerHTML = '';
    if (!msgs || msgs.length === 0) { box.innerHTML = '<div class="dim">nothing held</div>'; return; }
    msgs.forEach(m => {
      const el = document.createElement('div');
      el.className = 'held-item';
      el.innerHTML = '<div class="msg-head">' + m.from + ' · ' + m.id + '</div><div class="msg-body">' + (m.body || '') + '</div>';
      const actions = document.createElement('div'); actions.className = 'held-actions';
      const approve = document.createElement('button'); approve.className = 'btn primary'; approve.textContent = 'approve';
      approve.onclick = async () => { await fetchJSON('/api/settle', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({ids:[m.id], release:true})}); refreshHeld(); };
      const reject = document.createElement('button'); reject.className = 'btn'; reject.textContent = 'reject';
      reject.onclick = async () => { await fetchJSON('/api/settle', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({ids:[m.id], release:false})}); refreshHeld(); };
      actions.appendChild(approve); actions.appendChild(reject);
      el.appendChild(actions);
      box.appendChild(el);
    });
  } catch (e) { console.error('held failed', e); }
}

async function doSend() {
  const to = $('send-to').value.trim();
  const body = $('send-body').value.trim();
  const notify = $('send-notify').checked;
  if (!to || !body) { $('send-result').textContent = 'to and body required'; return; }
  const from = state.selected || 'cli:terminal';
  try {
    const r = await fetchJSON('/api/send', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({from, to, body, notify})});
    $('send-result').textContent = (r.recipients || []).map(x => '→ ' + x.address + ': ' + x.state + ' — ' + x.note).join('\n');
    $('send-body').value = '';
    refreshWho();
  } catch (e) { $('send-result').textContent = 'error: ' + e.message; }
}

async function doReply() {
  const body = $('reply-body').value.trim();
  if (!state.selected || !body) return;
  const to = state.selected.split('/').length ? state.selected : '';
  // reply goes back to the sender of the last message — use inbox data
  try {
    const i = await fetchJSON('/api/inbox?key=' + encodeURIComponent(state.selected));
    const last = i.messages && i.messages.length ? i.messages[i.messages.length-1] : null;
    const target = last ? last.from : state.selected;
    const r = await fetchJSON('/api/send', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({from: 'cli:terminal', to: target, body, notify: false})});
    $('reply-body').value = '';
    $('send-result').textContent = (r.recipients || []).map(x => '→ ' + x.address + ': ' + x.state).join('\n');
    refreshInbox();
  } catch (e) { console.error('reply failed', e); }
}

function startPolling() {
  if (state.timer) clearInterval(state.timer);
  state.timer = setInterval(() => {
    if (document.visibilityState === 'hidden') return;
    refreshWho();
    refreshHeld();
    if (state.selected) refreshInbox();
  }, 5000);
}

function startSSE() {
  if (typeof EventSource === 'undefined') return;
  try {
    state.es = new EventSource('/api/events');
    state.es.onmessage = (e) => {
      try { const d = JSON.parse(e.data); if (d.type === 'changed') { refreshWho(); refreshHeld(); if (state.selected) refreshInbox(); } } catch (_) {}
    };
    state.es.onerror = () => { /* browser auto-reconnects */ };
  } catch (e) { console.error('sse failed', e); }
}

document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible') { refreshWho(); refreshHeld(); if (state.selected) refreshInbox(); }
});

$('filter').addEventListener('input', refreshWho);
$('send-btn').addEventListener('click', doSend);
$('reply-send').addEventListener('click', doReply);
$('theme-toggle').addEventListener('click', () => {
  const html = document.documentElement;
  html.setAttribute('data-theme', html.getAttribute('data-theme') === 'dark' ? 'light' : 'dark');
  document.querySelector('meta[name="color-scheme"]').setAttribute('content', html.getAttribute('data-theme'));
});

// init
refreshWho();
refreshHeld();
startPolling();
startSSE();
