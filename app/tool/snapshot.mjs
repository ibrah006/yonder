// Saves a PNG of the running debug app: node tool/snapshot.mjs <vm-service ws url> <out.png>
// or taps what shows some text first:    node tool/snapshot.mjs <url> <out.png> --tap Terminal
// The url is the "wsUri" flutter run prints (flutter run --machine) or its
// "A Dart VM Service ... is available at" line, with /ws on the end.
const [url, out, flag, tap] = process.argv.slice(2);
const ws = new WebSocket(url);
let id = 0;
const pending = new Map();
const call = (method, params = {}) =>
  new Promise((resolve, reject) => {
    pending.set(++id, { resolve, reject });
    ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }));
  });
ws.onmessage = (e) => {
  const msg = JSON.parse(e.data);
  const p = pending.get(msg.id);
  if (!p) return;
  pending.delete(msg.id);
  msg.error ? p.reject(new Error(JSON.stringify(msg.error))) : p.resolve(msg.result);
};
ws.onopen = async () => {
  try {
    const vm = await call('getVM');
    const isolate = vm.isolates.find((i) => i.name.includes('main')) ?? vm.isolates[0];
    if (flag === '--tap') {
      await call('ext.yonder.tap', { isolateId: isolate.id, text: tap });
      await new Promise((r) => setTimeout(r, 1500));
    }
    await call('ext.yonder.snapshot', { isolateId: isolate.id, path: out });
    console.log(out);
    process.exit(0);
  } catch (err) {
    console.error(err.message);
    process.exit(1);
  }
};
