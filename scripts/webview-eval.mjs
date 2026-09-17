// Drive the Relay webview over the Chrome DevTools Protocol.
// usage: node cdp.mjs <port> <js-expression>
// Runs the expression in the page (awaiting it if it returns a promise) and
// prints the JSON result. In-page DOM clicks only; nothing reaches the desktop.
const [port, expr] = process.argv.slice(2);
const list = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
const page = list.find((t) => t.type === "page" && t.url.includes("localhost:1420"))
  ?? list.find((t) => t.type === "page");
if (!page) { console.error("no page target", list); process.exit(2); }
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
const result = await new Promise((resolve, reject) => {
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    if (d.id === 1) resolve(d);
  };
  ws.send(JSON.stringify({
    id: 1, method: "Runtime.evaluate",
    params: { expression: expr, awaitPromise: true, returnByValue: true },
  }));
  setTimeout(() => reject(new Error("timeout")), 10000);
});
ws.close();
if (result.result?.exceptionDetails) {
  console.error(JSON.stringify(result.result.exceptionDetails, null, 1));
  process.exit(1);
}
console.log(JSON.stringify(result.result?.result?.value ?? null));
