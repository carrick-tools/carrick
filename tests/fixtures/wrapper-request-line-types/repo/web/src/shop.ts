import { placeJson } from './net/json';
import { placeLoose } from './net/loose';
export const buyJson = () => placeJson('/orders', { body: 'book' });
export const buyLoose = () => placeLoose('/orders', 'book');
