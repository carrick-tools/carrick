import express from "express";

const app = express();
app.use(express.json());

interface User {
  id: string;
}

app.get("/users/:id", (req, res) => {
  res.json({ id: req.params.id } satisfies User);
});

app.post("/users", (req, res) => {
  res.json({ id: req.body.name } satisfies User);
});

app.get("/users/:id/status", (_req, res) => {
  res.json({ id: "u" } satisfies User);
});

app.get("/ping", (_req, res) => {
  res.json({ ok: true });
});

app.get("/users/:id/page", (_req, res) => {
  res.json({ items: [{ id: "u" }] });
});

app.get("/users/:id/model", (_req, res) => {
  res.json({ id: "u" } satisfies User);
});

app.post("/users/:id/frozen", (_req, res) => {
  res.json({ id: "u" } satisfies User);
});

app.get("/users/:id/then", (_req, res) => {
  res.json({ id: "u" } satisfies User);
});

app.post("/users/:id/caught", (_req, res) => {
  res.json({ id: "u" } satisfies User);
});

app.listen(4100);
