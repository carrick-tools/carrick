import Fastify from 'fastify';
import { itemRoutes } from './routes.js';

const server = Fastify();
server.register(itemRoutes, { prefix: '/api/v1' });
export default server;
