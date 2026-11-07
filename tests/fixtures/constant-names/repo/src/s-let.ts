import { Live } from '@fixture/live-wire';
let EVENT = 'room.left';
export function f() { new Live('wss://live.example/ws').emit(EVENT, { a: 1 }); }
