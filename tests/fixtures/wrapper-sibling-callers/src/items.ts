import { Connection } from './conn';

export class Items {
  constructor(
    private readonly connection: Connection,
    private readonly id: string
  ) {}

  async one() {
    return this.connection.send(`/api/items/${this.id}/one`, { cache: 'no-store' });
  }

  async two() {
    return this.connection.sendBytes(`/api/items/${this.id}/two`);
  }

  async three() {
    return this.connection.sendBytes('/api/items/three');
  }
}
