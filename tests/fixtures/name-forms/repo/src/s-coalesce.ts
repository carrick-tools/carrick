import { Live } from '@fixture/live-wire';
declare const maybe: string | undefined;
export function f() { new Live('wss://live.example/ws').emit(maybe || 'room.coalesced', { a: 1 }); }
