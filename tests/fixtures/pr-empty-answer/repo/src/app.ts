import createServer from "role-server";
import createClient from "role-fetcher";
import { mount } from "./typed";

const app = createServer();
const api = createClient();

// Two sites written alike: the same verb, the same literal, a path and one
// more argument. Only what each receiver is separates a route from a request.
app.get("/widgets", (req, res) => res.send("ok"));
api.get("/widgets", { retries: 2 });

mount(app);

// Routes registered in a loop over a tuple array: the paths are literals, but
// no single call site carries one.
const listed: Array<[string, string]> = [
  ["/orders", "orders"],
  ["/invoices", "invoices"],
];
for (const [path, name] of listed) {
  app.get(path, (req, res) => res.send(name));
}

// A data call nothing declares: a model candidate in the same file.
export function loadLedger(host: string): Promise<Response> {
  return fetch(`https://${host}/ledger`);
}
