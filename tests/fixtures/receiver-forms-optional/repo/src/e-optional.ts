import { Emitter } from '@fixture/ev';
export class Svc {
  constructor(private ev: Emitter) {}
  go() { this.ev?.emit('e.optional', {}); }
  listen() { this.ev?.on('e.optional-on', () => {}); }
}
