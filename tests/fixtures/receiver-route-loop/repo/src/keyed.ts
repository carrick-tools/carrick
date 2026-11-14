import server from 'fixture-plugin-server';
import store from 'fixture-plain-store';

const app = server();
const h = async () => ({ ok: true });

// A computed key that is one text naming an op.
const K = 'get' as const;
app[K]('/keyed/const', h);

// A computed key read off a loop over texts: several texts.
for (const v of ['get', 'post'] as const) app[v]('/keyed/loop', h);

// A computed key only its type states.
declare const m: 'get' | 'post';
app[m]('/keyed/typed', h);

// A computed key taken from a parameter, and the call that fills it.
function reg(k: 'get' | 'post') { app[k]('/keyed/param', h); }
reg('get');

// A computed key that is a choice.
app[Math.random() > 0.5 ? 'get' : 'post']('/keyed/choice', h);

// A computed key that is one text naming no op.
const AH = 'addHook' as const;
app[AH]('onRequest', h);

// A computed key on a receiver the store claims no op on.
const kv = store();
kv.open();
const OPEN = 'open' as const;
kv[OPEN]();
