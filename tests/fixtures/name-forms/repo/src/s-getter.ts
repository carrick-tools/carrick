import { Live } from '@fixture/live-wire';
class E { get name() { return 'room.getter'; } go() { new Live('wss://live.example/ws').emit(this.name, { a: 1 }); } }
export function f() { new E().go(); }
