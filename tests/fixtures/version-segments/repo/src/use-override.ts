import { sendV2 } from './client/override';
export const g = () => sendV2('/g', { version: 'v1' });
export const h = () => sendV2('/h', { body: 1 });
