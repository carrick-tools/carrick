import build from 'fixture-aliased-server';
import { catalogPlugin } from './catalog';
import { ping } from './handlers';

const app = build();

app.get('/health', ping);

// A plugin written in place: its routes are registered on the parameter.
app.register(
  async (scope, opts) => {
    scope.get('/status', ping);
  },
  { prefix: '/internal' },
);

// A plugin handed by name, with options it reads below.
app.register(catalogPlugin, { prefix: '/api', version: '/v2' });

app.listen(3000);
