import { report } from './net/reports';
declare function pick(): string;
export const daily = () => report('/daily');
export const picked = () => report(pick());
