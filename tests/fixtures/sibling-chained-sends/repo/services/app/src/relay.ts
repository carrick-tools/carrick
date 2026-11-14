import { pushLiteral, pushHeld, pushBoth, openOnly, closeChained, fetchChained, fetchHeld, postChained } from '@fx/kit';
export async function relayLiteral() { return pushLiteral(); }
export async function relayHeld() { return pushHeld(); }
export async function relayBoth() { return pushBoth(); }
export function relayOpenOnly() { return openOnly('kit-open'); }
export async function relayClose() { return closeChained(); }
export async function relayFetchChained() { return fetchChained(); }
export async function relayFetchHeld() { return fetchHeld(); }
export function relayPost() { return postChained(); }
