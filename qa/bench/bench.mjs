// Load harness — raw ws clients (light enough for ~1k conns), one scenario per run.
// Usage: node bench.mjs <resonance|reverb> <idle|fanout|sustained>
// Both servers speak Pusher, so only host/port change between targets.
import PusherServer from "pusher";
import WebSocket from "ws";
import { performance } from "node:perf_hooks";

const APP = { id: "app1", key: "resonance-key", secret: "resonance-secret" };
const TARGETS = {
  resonance: { wsPort: 8080, restPort: "8080" },
  reverb: { wsPort: 8081, restPort: "8081" },
};

const target = process.argv[2];
const scenario = process.argv[3];
const CONNS = Number(process.env.BENCH_CONNS || 1000);
const t = TARGETS[target];
if (!t || !scenario) { console.error("usage: node bench.mjs <resonance|reverb> <idle|fanout|sustained>"); process.exit(2); }

const server = new PusherServer({
  appId: APP.id, key: APP.key, secret: APP.secret,
  host: "127.0.0.1", port: t.restPort, useTLS: false,
});
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const url = `ws://127.0.0.1:${t.wsPort}/app/${APP.key}?protocol=7&client=bench&version=1`;

function pct(arr, q) {
  if (!arr.length) return null;
  const s = [...arr].sort((a, b) => a - b);
  return +s[Math.min(s.length - 1, Math.floor(q * s.length))].toFixed(2);
}

// One raw client. onEvent(evName, parsedData) fires for app events.
function connect(onEvent) {
  const ws = new WebSocket(url, { perMessageDeflate: false });
  const c = { ws, established: false, subscribed: false };
  ws.on("message", (buf) => {
    let f; try { f = JSON.parse(buf.toString()); } catch { return; }
    if (f.event === "pusher:connection_established") c.established = true;
    else if (f.event === "pusher_internal:subscription_succeeded") c.subscribed = true;
    else if (f.event === "pusher:ping") ws.send('{"event":"pusher:pong","data":"{}"}');
    else if (onEvent) { let d; try { d = JSON.parse(f.data); } catch { d = f.data; } onEvent(f.event, d); }
  });
  return c;
}

// Connect n clients in batches; resolve once all established.
async function connectAll(n, onEvent) {
  const clients = [];
  for (let i = 0; i < n; i += 100) {
    const batch = [];
    for (let j = i; j < Math.min(i + 100, n); j++) batch.push(connect(onEvent));
    clients.push(...batch);
    await Promise.all(batch.map((c) => new Promise((res, rej) => {
      const to = setTimeout(() => rej(new Error("connect timeout")), 15000);
      const iv = setInterval(() => { if (c.established) { clearInterval(iv); clearTimeout(to); res(); } }, 20);
    })));
  }
  return clients;
}

async function subscribeAll(clients, channel) {
  clients.forEach((c) => c.ws.send(JSON.stringify({ event: "pusher:subscribe", data: { channel } })));
  const deadline = performance.now() + 20000;
  while (clients.some((c) => !c.subscribed) && performance.now() < deadline) await wait(50);
  const ok = clients.filter((c) => c.subscribed).length;
  if (ok < clients.length) console.error(`  warn: ${ok}/${clients.length} subscribed`);
}

async function idle() {
  const clients = await connectAll(CONNS);
  console.log(`READY ${clients.length} connections established`);
  await wait(20000); // hold while the orchestrator samples docker stats
  process.exit(0);
}

async function fanout() {
  const latencies = [];
  let t0 = 0, received = 0;
  const clients = await connectAll(CONNS, (ev) => {
    if (ev === "boom") { latencies.push(performance.now() - t0); received++; }
  });
  await subscribeAll(clients, "bench");
  await wait(300);
  t0 = performance.now();
  await server.trigger("bench", "boom", { at: t0 });
  const deadline = performance.now() + 10000;
  while (received < clients.length && performance.now() < deadline) await wait(20);
  result({ scenario: "fanout", target, conns: clients.length, delivered: received,
    p50_ms: pct(latencies, 0.5), p99_ms: pct(latencies, 0.99), max_ms: pct(latencies, 1) });
  process.exit(0);
}

async function sustained() {
  const N = Math.min(CONNS, 500), RATE = 100, SECONDS = 5;
  const sendTimes = new Map();
  const latencies = [];
  let received = 0;
  const clients = await connectAll(N, (ev, d) => {
    if (ev === "tick" && d && sendTimes.has(d.seq)) { latencies.push(performance.now() - sendTimes.get(d.seq)); received++; }
  });
  await subscribeAll(clients, "bench");
  await wait(300);
  const total = RATE * SECONDS;
  const interval = 1000 / RATE;
  const start = performance.now();
  for (let seq = 0; seq < total; seq++) {
    sendTimes.set(seq, performance.now());
    server.trigger("bench", "tick", { seq }).catch(() => {});
    const nextAt = start + (seq + 1) * interval;
    const sleep = nextAt - performance.now();
    if (sleep > 0) await wait(sleep);
  }
  const expected = total * clients.length;
  const deadline = performance.now() + 8000;
  while (received < expected && performance.now() < deadline) await wait(20);
  result({ scenario: "sustained", target, conns: clients.length, msgs_sent: total,
    delivered: received, expected, delivery_pct: +(100 * received / expected).toFixed(1),
    p50_ms: pct(latencies, 0.5), p99_ms: pct(latencies, 0.99), max_ms: pct(latencies, 1) });
  process.exit(0);
}

function result(obj) {
  console.log("RESULT " + JSON.stringify(obj));
}

const runners = { idle, fanout, sustained };
runners[scenario]().catch((e) => { console.error("FATAL:", e.message); process.exit(1); });
