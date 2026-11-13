import { ping } from './net/ping';
export const check = () => ping('/ping');
