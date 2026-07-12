<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="csrf-token" content="{{ csrf_token() }}">
<title>Ripple — dashboard</title>
<style>
  :root { --bg:#0d1117; --card:#161b22; --border:#21262d; --fg:#e6edf3; --muted:#8b949e;
          --accent:#39a0ed; --good:#3fb950; --bad:#f85149; --warn:#d29922; }
  * { box-sizing:border-box; }
  body { margin:0; background:var(--bg); color:var(--fg); font:14px/1.5 -apple-system,Segoe UI,Roboto,sans-serif; }
  .wrap { max-width:1100px; margin:0 auto; padding:24px; }
  header { display:flex; align-items:center; gap:12px; margin-bottom:20px; }
  header h1 { font-size:20px; margin:0; }
  .dot { width:10px; height:10px; border-radius:50%; background:var(--muted); }
  .dot.up { background:var(--good); box-shadow:0 0 8px var(--good); }
  .dot.down { background:var(--bad); box-shadow:0 0 8px var(--bad); }
  .server { color:var(--muted); font-size:13px; }
  .spacer { flex:1; }
  button { background:var(--accent); color:#04121f; border:0; padding:8px 14px; border-radius:6px;
           font-weight:600; cursor:pointer; }
  button.ghost { background:transparent; color:var(--muted); border:1px solid var(--border); font-weight:400; }
  .banner { background:#3a1d1d; border:1px solid var(--bad); color:#ffb4ae; padding:10px 14px;
            border-radius:8px; margin-bottom:16px; display:none; }
  .cards { display:grid; grid-template-columns:repeat(auto-fit,minmax(160px,1fr)); gap:12px; margin-bottom:24px; }
  .card { background:var(--card); border:1px solid var(--border); border-radius:10px; padding:16px; }
  .card .label { color:var(--muted); font-size:12px; text-transform:uppercase; letter-spacing:.04em; }
  .card .value { font-size:28px; font-weight:700; margin-top:6px; }
  .card.alert .value { color:var(--warn); }
  table { width:100%; border-collapse:collapse; background:var(--card); border:1px solid var(--border); border-radius:10px; overflow:hidden; }
  th,td { text-align:left; padding:10px 14px; border-bottom:1px solid var(--border); }
  th { color:var(--muted); font-size:12px; text-transform:uppercase; letter-spacing:.04em; }
  tr:last-child td { border-bottom:0; }
  .badge { font-size:11px; padding:2px 8px; border-radius:20px; border:1px solid var(--border); color:var(--muted); }
  .badge.private { color:var(--warn); border-color:var(--warn); }
  .badge.presence { color:var(--accent); border-color:var(--accent); }
  .foot { color:var(--muted); font-size:12px; margin-top:14px; display:flex; align-items:center; gap:12px; }
  .empty { color:var(--muted); padding:24px; text-align:center; }
  .toast { margin-left:10px; font-size:13px; }
</style>
</head>
<body>
<div class="wrap">
  <header>
    <span id="dot" class="dot"></span>
    <h1>Ripple</h1>
    <span class="server">{{ $server }}</span>
    <span class="spacer"></span>
    <button id="test">Send test broadcast</button>
    <span id="toast" class="toast"></span>
  </header>

  <div id="banner" class="banner"></div>

  <div class="cards">
    <div class="card"><div class="label">Connections</div><div class="value" data-m="ripple_connections">–</div></div>
    <div class="card"><div class="label">Channels</div><div class="value" data-m="ripple_channels">–</div></div>
    <div class="card"><div class="label">Events received</div><div class="value" data-m="ripple_events_received_total">–</div></div>
    <div class="card"><div class="label">Messages sent</div><div class="value" data-m="ripple_messages_sent_total">–</div></div>
    <div class="card" id="killcard"><div class="label">Slow-consumer kills</div><div class="value" data-m="ripple_slow_consumers_killed_total">–</div></div>
    <div class="card"><div class="label">Last fan-out</div><div class="value" id="fanout">–</div></div>
  </div>

  <table>
    <thead><tr><th>Channel</th><th>Type</th><th>Subscriptions</th><th>Presence users</th></tr></thead>
    <tbody id="rows"><tr><td colspan="4" class="empty">Loading…</td></tr></tbody>
  </table>

  <div class="foot">
    <button class="ghost" id="toggle">Pause</button>
    <span id="updated"></span>
  </div>
</div>

<script>
const STATS = @json($statsUrl), TEST = @json($testUrl);
const CSRF = document.querySelector('meta[name=csrf-token]').content;
let live = true;

function fmt(n) { return n == null ? '–' : Number(n).toLocaleString(); }

async function refresh() {
  try {
    const r = await fetch(STATS, { headers: { 'Accept': 'application/json' } });
    const d = await r.json();
    const up = d.health.reachable;
    document.getElementById('dot').className = 'dot ' + (up ? 'up' : 'down');

    const banner = document.getElementById('banner');
    if (d.health.error) { banner.style.display = 'block'; banner.textContent = '⚠ ' + d.health.error; }
    else banner.style.display = 'none';

    document.querySelectorAll('[data-m]').forEach(el => el.textContent = fmt(d.metrics[el.dataset.m]));
    const us = d.metrics['ripple_last_fanout_us'], tg = d.metrics['ripple_last_fanout_targets'];
    document.getElementById('fanout').textContent = us ? (us/1000).toFixed(1) + ' ms / ' + fmt(tg) : '–';
    document.getElementById('killcard').className = 'card' + (d.metrics['ripple_slow_consumers_killed_total'] > 0 ? ' alert' : '');

    const rows = document.getElementById('rows');
    if (!d.channels.length) {
      rows.innerHTML = '<tr><td colspan="4" class="empty">' + (up ? 'No occupied channels' : 'Server unreachable') + '</td></tr>';
    } else {
      rows.innerHTML = d.channels.map(c =>
        `<tr><td>${c.name}</td><td><span class="badge ${c.type}">${c.type}</span></td>`
        + `<td>${fmt(c.subscription_count)}</td><td>${c.user_count == null ? '—' : fmt(c.user_count)}</td></tr>`
      ).join('');
    }
    document.getElementById('updated').textContent = 'Updated ' + new Date().toLocaleTimeString();
  } catch (e) {
    document.getElementById('dot').className = 'dot down';
  }
}

document.getElementById('test').onclick = async (ev) => {
  ev.target.disabled = true;
  const toast = document.getElementById('toast');
  try {
    const r = await fetch(TEST, { method: 'POST', headers: { 'X-CSRF-TOKEN': CSRF, 'Accept': 'application/json' } });
    const d = await r.json();
    toast.style.color = d.sent ? 'var(--good)' : 'var(--bad)';
    toast.textContent = d.sent ? '✓ broadcast sent (channel ripple-dashboard-test)' : ('✗ ' + (d.error || ('HTTP ' + d.status)));
  } catch (e) { toast.style.color = 'var(--bad)'; toast.textContent = '✗ ' + e.message; }
  setTimeout(() => toast.textContent = '', 5000);
  ev.target.disabled = false;
  refresh();
};

document.getElementById('toggle').onclick = (ev) => {
  live = !live; ev.target.textContent = live ? 'Pause' : 'Resume';
};

refresh();
setInterval(() => { if (live) refresh(); }, 2000);
</script>
</body>
</html>
