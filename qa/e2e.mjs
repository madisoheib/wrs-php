// End-to-end compat check against a running resonance server, driven by the
// REAL Pusher client libs (spec §4.6): pusher-js subscribes, the `pusher`
// server lib signs private-channel auth AND the REST trigger exactly like
// pusher-php-server does. If these pass, a real Laravel/Echo app works too.
import PusherPkg from "pusher-js";
import PusherServer from "pusher";
import WebSocket from "ws";

const Pusher = PusherPkg.Pusher || PusherPkg.default || PusherPkg; // CJS/ESM interop

globalThis.WebSocket = WebSocket; // pusher-js runtime in Node

const HOST = process.env.RESONANCE_HOST || "127.0.0.1";
const PORT = Number(process.env.RESONANCE_PORT || 8080);
const APP = { id: "app1", key: "resonance-key", secret: "resonance-secret" };

const server = new PusherServer({
  appId: APP.id, key: APP.key, secret: APP.secret,
  host: HOST, port: String(PORT), useTLS: false,
});

const client = new Pusher(APP.key, {
  wsHost: HOST, wsPort: PORT, forceTLS: false, disableStats: true,
  enabledTransports: ["ws"], cluster: "mt1",
  // Mirrors Laravel's /broadcasting/auth: server-side HMAC of socket_id:channel.
  authorizer: (channel) => ({
    authorize: (socketId, cb) => cb(null, server.authorizeChannel(socketId, channel.name)),
  }),
});

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
function receive(ch, event) {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`timeout waiting ${event} on ${ch.name}`)), 5000);
    ch.bind(event, (data) => { clearTimeout(t); resolve(data); });
  });
}
function subscribed(ch) {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`subscribe timeout ${ch.name}`)), 5000);
    ch.bind("pusher:subscription_succeeded", () => { clearTimeout(t); resolve(); });
    ch.bind("pusher:subscription_error", (e) => { clearTimeout(t); reject(new Error(`sub error ${ch.name}: ${JSON.stringify(e)}`)); });
  });
}

const results = [];
async function step(name, fn) {
  try { await fn(); results.push([name, true]); console.log(`  ✓ ${name}`); }
  catch (e) { results.push([name, false]); console.log(`  ✗ ${name}: ${e.message}`); }
}

async function main() {
  await new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error("connection timeout")), 8000);
    client.connection.bind("connected", () => { clearTimeout(t); resolve(); });
    client.connection.bind("error", (e) => { clearTimeout(t); reject(new Error(JSON.stringify(e))); });
  });
  console.log(`connected socket_id=${client.connection.socket_id}`);

  await step("public: subscribe + receive broadcast", async () => {
    const ch = client.subscribe("news");
    await subscribed(ch);
    const got = receive(ch, "update");
    await wait(100);
    await server.trigger("news", "update", { hello: "world" });
    const data = await got;
    if (data.hello !== "world") throw new Error(`bad payload ${JSON.stringify(data)}`);
  });

  await step("private: HMAC auth + receive broadcast", async () => {
    const ch = client.subscribe("private-room");
    await subscribed(ch);
    const got = receive(ch, "msg");
    await wait(100);
    await server.trigger("private-room", "msg", { n: 42 });
    const data = await got;
    if (data.n !== 42) throw new Error(`bad payload ${JSON.stringify(data)}`);
  });

  await step("REST reachable + returns 200", async () => {
    // trigger throws on non-2xx; a fresh channel with no subscribers still 200s.
    await server.trigger("empty-channel", "x", { ok: true });
  });

  await step("triggerBatch delivers to multiple channels", async () => {
    const c1 = client.subscribe("batch-a");
    const c2 = client.subscribe("batch-b");
    await Promise.all([subscribed(c1), subscribed(c2)]);
    const got1 = receive(c1, "ev");
    const got2 = receive(c2, "ev");
    await wait(100);
    await server.triggerBatch([
      { channel: "batch-a", name: "ev", data: JSON.stringify({ n: 1 }) },
      { channel: "batch-b", name: "ev", data: JSON.stringify({ n: 2 }) },
    ]);
    const [d1, d2] = await Promise.all([got1, got2]);
    if (d1.n !== 1 || d2.n !== 2) throw new Error(`bad payloads ${JSON.stringify([d1, d2])}`);
  });

  await step("GET /channels + /channels/{name} inspection", async () => {
    const res = await server.get({ path: "/channels", params: {} });
    const body = await res.json();
    if (!("news" in body.channels)) throw new Error(`news missing: ${JSON.stringify(body)}`);
    const one = await (await server.get({ path: "/channels/news", params: {} })).json();
    if (one.occupied !== true || one.subscription_count < 1) throw new Error(JSON.stringify(one));
    const empty = await (await server.get({ path: "/channels/ghost-channel", params: {} })).json();
    if (empty.occupied !== false) throw new Error(JSON.stringify(empty));
  });

  client.disconnect();
  const failed = results.filter(([, ok]) => !ok);
  console.log(`\n${results.length - failed.length}/${results.length} passed`);
  process.exit(failed.length ? 1 : 0);
}

main().catch((e) => { console.error("FATAL:", e.message); process.exit(1); });
