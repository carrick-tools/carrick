type Listener<T> = (value: T) => void;

export class LocalBus<T> {
  private readonly listeners: Listener<T>[] = [];

  listen(listener: Listener<T>): void {
    this.listeners.push(listener);
  }

  next(value: T): void {
    this.listeners.forEach((listener) => listener(value));
  }
}
