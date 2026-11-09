import { send } from '@fixture/send';
export class C { f = send; go() { return this.f('/p/t-field-init'); } }
