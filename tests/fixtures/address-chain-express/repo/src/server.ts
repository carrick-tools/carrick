import { server } from 'fixture-chain-server';
import { API_PREFIX } from './config';

const app = server();
const orders = server();
const admin = server();

orders.get('/orders/:id', async () => ({ id: 1 }));
admin.get('/stats', async () => ({ count: 0 }));
app.get('/health', async () => ({ ok: true }));

// The prefix is a constant another file declares.
app.mount(API_PREFIX, orders);
// The prefix is set when the process starts.
app.mount(process.env.ADMIN_BASE, admin);

app.listen(3000);
