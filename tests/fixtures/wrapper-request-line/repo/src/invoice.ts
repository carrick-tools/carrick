import { charge } from './net/billing';
export const bill = () => charge('/charges');
