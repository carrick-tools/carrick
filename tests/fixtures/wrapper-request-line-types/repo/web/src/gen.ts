import { placeGeneric } from './net/generic';
export const buyGeneric = () => placeGeneric('/orders', 'book');
export const relayGeneric = <U>(item: U) => placeGeneric('/orders', item);
export const anyGeneric = placeGeneric(process.argv[2], 'book');
