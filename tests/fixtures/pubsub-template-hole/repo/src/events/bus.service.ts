import { Hub } from "@fixture/ev";
import { Injectable } from "../framework";

// The service's own wrapper over the bus: the hub is made when the module
// starts, and every topic is the caller's.
@Injectable()
export class BusService {
  private hub: Hub;

  onModuleInit() {
    this.hub = new Hub();
  }

  listen<T>(topic: string): AsyncIterator<T> {
    return this.hub.listen<T>(topic);
  }

  async publish(topic: string, payload: unknown) {
    await this.hub.publish(topic, payload);
  }
}
