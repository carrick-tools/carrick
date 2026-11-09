import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
let ROUTE = "/s-route-let-written";
export function moveRoute(): void {
  ROUTE = "/s-route-let-moved";
}
createServer((req, res) => {
  if (req.method === "GET" && req.url === ROUTE) { res.end("a"); }
});
