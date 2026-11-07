import { Live } from '@fixture/live-wire';
const enum Ev { Joined = 'room.joined' }
export function f() { new Live('wss://live.example/ws').emit(Ev.Joined, { a: 1 }); }
