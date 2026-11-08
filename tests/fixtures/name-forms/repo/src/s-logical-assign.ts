import { Live } from '@fixture/live-wire';
let room: string | undefined;
export function f() { room ??= 'room.logical-assign'; new Live('wss://live.example/ws').emit(room, { a: 1 }); }
