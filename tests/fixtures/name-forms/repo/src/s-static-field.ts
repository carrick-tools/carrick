import { Live } from '@fixture/live-wire';
class E { static NAME = 'room.static-field'; }
export function f() { new Live('wss://live.example/ws').emit(E.NAME, { a: 1 }); }
