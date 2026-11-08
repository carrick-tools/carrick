import { Live } from '@fixture/live-wire';
function announce(event = 'room.defaulted') { new Live('wss://live.example/ws').emit(event, { a: 1 }); }
export function f() { announce(); }
