import { App } from "res-kit";
import { ping } from "./handlers";

export interface Item { id: string; label: string }
interface NewWidget { name: string; size: number }

declare function save(body: unknown): Promise<{ ok: boolean; item: Item }>;
declare function load(): Promise<Item>;

const app = new App();

// naïve café ✓ 😀: non-ASCII text ahead of every value below

// A success send and an error send: only the located success payload is read.
app.get('/two-success', async (req, res) => {
  const result = await save(req.body);
  if (!result.ok) {
    res.status(400).json({ bad: true });
    return;
  }
  if (result.item.label) {
    res.status(201).json({ made: result.item.label });
    return;
  }
  res.json({ none: true });
});

// A body read as a typed value, and a second access nothing was built from.
app.post('/read', async (req, res) => {
  const body = (await req.json()) as NewWidget;
  const again = req.body.name;
  res.json({ name: body.name, again });
});

// A handler declared in another file.
app.get('/imported', ping);

// A model-stated type for this route's response wins over the reader's.
app.get('/stated', async (): Promise<Item> => load());
