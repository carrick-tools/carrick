import { call } from './client/runtime';
declare const current: () => string;
export const r1 = () => call('/r/one', { version: current() });
export const r2 = () => call('/r/two');
export const r3 = () => call('/r/three', { version: 'v2' });
