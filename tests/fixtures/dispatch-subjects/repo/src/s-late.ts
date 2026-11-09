import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
createServer((req, res) => {
  let url: URL;
  try {
    url = new URL(req.url ?? "/", "http://localhost");
  } catch {
    res.end("bad");
    return;
  }
  if (req.method === "GET" && url.pathname === "/s-late") { res.end("a"); }
});
