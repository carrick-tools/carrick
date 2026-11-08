import { Live } from '@fixture/live-wire';
const room = 'room.fallback-stated';
export function f() { new Live('wss://live.example/ws').emit(room || 'room.fallback-stated', { a: 1 }); }
