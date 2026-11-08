import { Live } from '@fixture/live-wire';
let event = '';
({ k: event } = { k: 'room.destr-assign' });
export function f() { new Live('wss://live.example/ws').emit(event, { a: 1 }); }
