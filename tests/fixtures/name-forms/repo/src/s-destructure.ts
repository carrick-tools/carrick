import { Live } from '@fixture/live-wire';
const { event } = { event: 'room.destructured' };
export function f() { new Live('wss://live.example/ws').emit(event, { a: 1 }); }
