// Raw-wire protocol/behaviour tests: things the happy-path lib test can't easily
// reach — sender exclusion, unsubscribe, slow-consumer kill, fan-out, errors.
// REST triggers still go through the real `pusher` lib (correct signing).
import PusherServer from "pusher";
import WebSocket from "ws";

const HOST = process.env.RESONANCE_HOST || "127.0.0.1";
const PORT = Number(process.env.RESONANCE_PORT || 8080);
const APP = { id: "app1", key: "resonance-key", secret: "resonance-secret" };

const server = new PusherServer({
  appId: APP.id, key: APP.key, secret: APP.secret,
  host: HOST, port: String(PORT), useTLS: false,
});

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const safeParse = (s) => { try { return JSON.parse(s); } catch { return s; } };

function client() {
  const ws = new WebSocket(`ws://${HOST}:${PORT}/app/${APP.key}?protocol=7&client=raw&version=1`);
  const frames = [];
  const waiters = [];
  let opened = false, openErr = null;
  const openWaiters = [];
  ws.on("open", () => { opened = true; openWaiters.forEach((w) => w.res()); });
  ws.on("error", (e) => { openErr = e; openWaiters.forEach((w) => w.rej(e)); });
  ws.on("message", (buf) => {
    const f = JSON.parse(buf.toString());
    f.parsed = f.data !== undefined ? safeParse(f.data) : undefined;
    frames.push(f);
    for (const w of [...waiters]) {
      if (w.pred(f)) { waiters.splice(waiters.indexOf(w), 1); clearTimeout(w.t); w.resolve(f); }
    }
  });
  const api = {
    ws, frames,
    open: () => new Promise((res, rej) => {
      if (opened) return res();
      if (openErr) return rej(openErr);
      const t = setTimeout(() => rej(new Error("open timeout")), 3000);
      openWaiters.push({ res: () => { clearTimeout(t); res(); }, rej: (e) => { clearTimeout(t); rej(e); } });
    }),
    send: (o) => ws.send(JSON.stringify(o)),
    waitFor: (pred, label = "", ms = 3000) => new Promise((res, rej) => {
      const hit = frames.find(pred);
      if (hit) return res(hit);
      const t = setTimeout(() => {
        const i = waiters.findIndex((x) => x.t === t);
        if (i >= 0) waiters.splice(i, 1);
        rej(new Error(`timeout: ${label}`));
      }, ms);
      waiters.push({ pred, resolve: res, t });
    }),
    has: (pred) => frames.some(pred),
    close: () => ws.close(),
  };
  return api;
}

async function establish(c) {
  await c.open();
  const est = await c.waitFor((f) => f.event === "pusher:connection_established", "connection_established");
  return est;
}

const results = [];
async function step(name, fn) {
  try { await fn(); results.push([name, true]); console.log(`  ✓ ${name}`); }
  catch (e) { results.push([name, false]); console.log(`  ✗ ${name}: ${e.message}`); }
}

async function main() {
  await step("handshake: connection_established with string-encoded data", async () => {
    const c = client();
    const est = await establish(c);
    if (typeof est.data !== "string") throw new Error("data must be a JSON-encoded string (double-encoding)");
    if (!est.parsed.socket_id || !est.parsed.activity_timeout) throw new Error("missing socket_id/activity_timeout");
    c.close();
  });

  await step("pusher:ping -> pusher:pong", async () => {
    const c = client();
    await establish(c);
    c.send({ event: "pusher:ping" });
    await c.waitFor((f) => f.event === "pusher:pong", "pong");
    c.close();
  });

  await step("private subscribe with bad auth -> pusher:error 4009", async () => {
    const c = client();
    await establish(c);
    c.send({ event: "pusher:subscribe", data: { channel: "private-x", auth: "resonance-key:deadbeef" } });
    const err = await c.waitFor((f) => f.event === "pusher:error", "error");
    if (err.parsed.code !== 4009) throw new Error(`expected 4009, got ${err.parsed.code}`);
    if (c.has((f) => f.event === "pusher_internal:subscription_succeeded")) throw new Error("must not subscribe");
    c.close();
  });

  await step("sender exclusion: trigger with socket_id skips the sender", async () => {
    const a = client(); const b = client();
    const ea = await establish(a); await establish(b);
    for (const c of [a, b]) {
      c.send({ event: "pusher:subscribe", data: { channel: "excl" } });
      await c.waitFor((f) => f.event === "pusher_internal:subscription_succeeded" && f.channel === "excl", "sub");
    }
    await server.trigger("excl", "evt", { hi: 1 }, { socket_id: ea.parsed.socket_id });
    await b.waitFor((f) => f.event === "evt" && f.channel === "excl", "b receives");
    await wait(200);
    if (a.has((f) => f.event === "evt")) throw new Error("sender A should have been excluded");
    a.close(); b.close();
  });

  await step("unsubscribe stops delivery", async () => {
    const c = client();
    await establish(c);
    c.send({ event: "pusher:subscribe", data: { channel: "u" } });
    await c.waitFor((f) => f.event === "pusher_internal:subscription_succeeded" && f.channel === "u", "sub");
    c.send({ event: "pusher:unsubscribe", data: { channel: "u" } });
    await wait(100);
    await server.trigger("u", "evt", { x: 1 });
    await wait(400);
    if (c.has((f) => f.event === "evt")) throw new Error("received after unsubscribe");
    c.close();
  });

  await step("fan-out: 1 event -> 30 subscribers", async () => {
    const clients = Array.from({ length: 30 }, () => client());
    await Promise.all(clients.map(establish));
    await Promise.all(clients.map(async (c) => {
      c.send({ event: "pusher:subscribe", data: { channel: "fan" } });
      await c.waitFor((f) => f.event === "pusher_internal:subscription_succeeded" && f.channel === "fan", "sub");
    }));
    await server.trigger("fan", "boom", { seq: 7 });
    await Promise.all(clients.map((c) => c.waitFor((f) => f.event === "boom" && f.parsed.seq === 7, "recv", 4000)));
    clients.forEach((c) => c.close());
  });

  await step("slow-consumer kill: stalled reader is disconnected under flood", async () => {
    const c = client();
    await establish(c);
    c.send({ event: "pusher:subscribe", data: { channel: "flood" } });
    await c.waitFor((f) => f.event === "pusher_internal:subscription_succeeded" && f.channel === "flood", "sub");
    const closed = new Promise((res) => c.ws.on("close", () => res(true)));
    c.ws._socket.pause(); // stop draining TCP -> kernel buffers then mpsc fill up
    const big = "x".repeat(10000);
    // Enough volume (~15MB) to overflow loopback socket buffers so the server's
    // writer blocks, its bounded mpsc(64) fills, try_send fails -> kill.
    await Promise.allSettled(
      Array.from({ length: 1500 }, (_, i) => server.trigger("flood", "spam", { i, big }))
    );
    c.ws._socket.resume(); // now node can process the FIN and emit 'close'
    const killed = await Promise.race([closed, wait(4000).then(() => false)]);
    if (!killed) throw new Error("slow consumer was not disconnected");
  });

  await step("presence: join roster, member_added, member_removed", async () => {
    // authorizeChannel with presence data — same as Laravel's /broadcasting/auth
    const a = client(); const b = client();
    const ea = await establish(a); const eb = await establish(b);
    const chName = "presence-room";
    const authA = server.authorizeChannel(ea.parsed.socket_id, chName, { user_id: "u1", user_info: { name: "Alice" } });
    const authB = server.authorizeChannel(eb.parsed.socket_id, chName, { user_id: "u2", user_info: { name: "Bob" } });

    a.send({ event: "pusher:subscribe", data: { channel: chName, auth: authA.auth, channel_data: authA.channel_data } });
    const okA = await a.waitFor((f) => f.event === "pusher_internal:subscription_succeeded" && f.channel === chName, "sub A");
    if (okA.parsed.presence.count !== 1 || !okA.parsed.presence.ids.includes("u1")) throw new Error(`bad roster A: ${okA.data}`);

    b.send({ event: "pusher:subscribe", data: { channel: chName, auth: authB.auth, channel_data: authB.channel_data } });
    const okB = await b.waitFor((f) => f.event === "pusher_internal:subscription_succeeded" && f.channel === chName, "sub B");
    if (okB.parsed.presence.count !== 2) throw new Error(`bad roster B: ${okB.data}`);

    const added = await a.waitFor((f) => f.event === "pusher_internal:member_added", "member_added");
    if (added.parsed.user_id !== "u2") throw new Error("wrong member_added");

    b.send({ event: "pusher:unsubscribe", data: { channel: chName } });
    const removed = await a.waitFor((f) => f.event === "pusher_internal:member_removed", "member_removed");
    if (removed.parsed.user_id !== "u2") throw new Error("wrong member_removed");
    a.close(); b.close();
  });

  await step("presence: bad signature rejected", async () => {
    const c = client();
    const est = await establish(c);
    c.send({ event: "pusher:subscribe", data: { channel: "presence-x", auth: "resonance-key:deadbeef", channel_data: '{"user_id":"u9"}' } });
    const err = await c.waitFor((f) => f.event === "pusher:error", "error");
    if (err.parsed.code !== 4009) throw new Error(`expected 4009, got ${err.parsed.code}`);
    c.close();
  });

  await step("client events: relayed to peers, not sender, private only", async () => {
    const a = client(); const b = client();
    const ea = await establish(a); const eb = await establish(b);
    const ch = "private-chat";
    for (const [c, est] of [[a, ea], [b, eb]]) {
      const auth = server.authorizeChannel(est.parsed.socket_id, ch);
      c.send({ event: "pusher:subscribe", data: { channel: ch, auth: auth.auth } });
      await c.waitFor((f) => f.event === "pusher_internal:subscription_succeeded" && f.channel === ch, "sub");
    }
    a.send({ event: "client-typing", channel: ch, data: { user: "alice" } });
    const got = await b.waitFor((f) => f.event === "client-typing", "client event on B");
    if (got.data.user !== "alice") throw new Error(`bad payload: ${JSON.stringify(got.data)}`);
    await wait(200);
    if (a.has((f) => f.event === "client-typing")) throw new Error("echoed back to sender");
    // not allowed on public channels
    a.send({ event: "pusher:subscribe", data: { channel: "pub" } });
    await a.waitFor((f) => f.event === "pusher_internal:subscription_succeeded" && f.channel === "pub", "sub pub");
    a.send({ event: "client-nope", channel: "pub", data: {} });
    const err = await a.waitFor((f) => f.event === "pusher:error", "public refusal");
    if (err.parsed.code !== 4009) throw new Error("public channel client event not refused");
    a.close(); b.close();
  });

  await step("client events: rate limited (>10/s -> 4301)", async () => {
    const a = client(); const b = client();
    const ea = await establish(a); await establish(b);
    const ch = "private-flood";
    const auth = server.authorizeChannel(ea.parsed.socket_id, ch);
    a.send({ event: "pusher:subscribe", data: { channel: ch, auth: auth.auth } });
    await a.waitFor((f) => f.event === "pusher_internal:subscription_succeeded" && f.channel === ch, "sub");
    for (let i = 0; i < 15; i++) a.send({ event: "client-spam", channel: ch, data: { i } });
    const err = await a.waitFor((f) => f.event === "pusher:error" && f.parsed.code === 4301, "rate limit error");
    if (!err) throw new Error("no rate limit");
    a.close(); b.close();
  });

  const failed = results.filter(([, ok]) => !ok);
  console.log(`\n${results.length - failed.length}/${results.length} passed`);
  process.exit(failed.length ? 1 : 0);
}

main().catch((e) => { console.error("FATAL:", e.message); process.exit(1); });
