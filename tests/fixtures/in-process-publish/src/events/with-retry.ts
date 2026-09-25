type Constructor<T> = new (...args: any[]) => T;

export function withRetry<T extends Constructor<object>>(Base: T) {
  return class extends Base {
    attempts = 3;
  };
}
