import { keyed } from './client/param-key';
export const k1 = () => keyed('/k/omitted');
export const k2 = () => keyed('/k/empty', {});
export const k3 = () => keyed('/k/given', { version: 'v2' });
