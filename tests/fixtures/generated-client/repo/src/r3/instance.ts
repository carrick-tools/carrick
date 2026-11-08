import { createClient } from './client/client';
import { createConfig } from './client/utils';

export const client = createClient(createConfig({ baseUrl: 'http://localhost:4000' }));
