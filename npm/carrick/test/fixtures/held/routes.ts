import { Hono } from "hono";

const app = new Hono();
const listOrders = (c: any) => c.json({ orders: [] as string[] });
const readA = (c: any) => c.json({ a: 1 });
const writeB = (c: any) => c.json({ b: 2 });
const first = (c: any) => c.json({ n: 1 });
const second = (c: any) => c.json({ n: 2 });

app.get("/orders", listOrders);
app.get("/a", readA).post("/b", writeB);
app.get("/dup", first);
app.get("/dup", second);

export default app;
