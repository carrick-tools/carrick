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
