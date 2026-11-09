import { send } from '@fixture/send';
export class C { g = send; f = this.g; go() { return this.f('/p/t-field-copy'); } }
