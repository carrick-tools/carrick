import { server } from 'fixture-chain-server';

const app = server();

// A prefix this module assigns once: the compiler types it `string`, the
// fold reads the one text it holds.
let BASE = '/api';
// An enum member: the compiler types it as the one string it holds.
enum Route {
  Items = '/items',
}

// An entry of a constant object, read by name and by a computed key.
const ROUTES = { things: '/things', other: '/other' };
const KEY = 'other';

app.get(`${BASE}/items`, async () => ({ ok: true }));
app.get(Route.Items, async () => ({ ok: true }));

app.get(ROUTES.things, async () => ({ ok: true }));
app.get(ROUTES[KEY], async () => ({ ok: true }));

app.listen(3000);
