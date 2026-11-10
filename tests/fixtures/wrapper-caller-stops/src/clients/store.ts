export abstract class Store {
  protected abstract endpoint: string;

  list(): Promise<Response> {
    return fetch(`/${this.endpoint}.list`);
  }
}

export class ItemStore extends Store {
  protected endpoint = "items";
}
