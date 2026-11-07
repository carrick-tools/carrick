import { Inst } from '@fixture/mk';
export class A { constructor(private api: Inst) {} f() { return this.api.get('/p/m-param-property-di'); } }
