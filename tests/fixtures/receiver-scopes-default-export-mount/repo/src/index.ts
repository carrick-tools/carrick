import server from 'fixture-mount-server';

// Both apps are made through the package's `default` export; the store
// labels the mount member on the named export alone.
const app = server();
const users = server();

users.get('/list', async () => ({ users: [] }));

app.get('/health', async () => ({ ok: true }));
app.mount('/v1/users', users);

app.listen(3000);
