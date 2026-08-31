import { router } from './framework';
import errorHandler from './middleware/error-handler';
import root from './controllers/root';
import token from './controllers/token';
import widget from './controllers/widget';
import widgetItem from './controllers/widget-item';
import report from './controllers/report';
import session from './controllers/session';
import profile from './controllers/profile';
import health from './controllers/health';

// The route table. Every path lives here and nowhere else: the controller
// modules never name their own path, so single-file analysis cannot see it.
export default [
  router('/', root),
  // Middleware in front of the controller — the controller is still the last
  // handler argument.
  router('/token', errorHandler, token),
  router('/widget', widget),
  router('/widget/:id', widgetItem),
  router('/report', report),
  router('/session', session),
  router('/profile', profile),
  router('/health', health),
];
