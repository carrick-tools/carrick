import { Hono } from "hono";
import { v } from "dupe";
import { bv } from "pkg-b";
import { limit, toBuf } from "./helper.js";

const a: "one" = v;
const b: "two" = bv;
const wrongVersion: "two" = v; // own error: `dupe` resolves to the root copy here
const notANumber: number = "x"; // own error
const total: number = limit + toBuf("a").length;

const app = new Hono();
app.get("/orders", (c) => c.json({ total, a, b }));
app.post("/orders", (c) => c.json({ created: true }, 201));
export default app;
