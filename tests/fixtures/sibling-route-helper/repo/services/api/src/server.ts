import { server } from 'fixture-route-server';
import { addRoute } from '../../../packages/kit/src/route';
import { pingRoute } from './routes/ping';

const app = server();

addRoute(app, pingRoute);

app.listen(3000);
