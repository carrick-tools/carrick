import { Live } from '@fixture/live-wire';
declare const flag: boolean;
export function f() { new Live('wss://live.example/ws').emit(flag ? 'room.conditional-a' : 'room.conditional-b', { a: 1 }); }
