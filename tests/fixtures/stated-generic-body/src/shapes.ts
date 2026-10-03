// The bodies the client reads, declared away from the reads.
export interface Order {
  id: string;
  total: number;
}

export interface Member {
  id: string;
  name: string;
}

// What every list read comes back wrapped in.
export interface Envelope<T> {
  data: T;
  cursor: string | null;
}

// One instantiation, given a name of its own.
export type OrderEnvelope = Envelope<Order[]>;

// A generic whose argument has a default.
export interface Listing<T = Order> {
  items: T[];
  total: number;
}
