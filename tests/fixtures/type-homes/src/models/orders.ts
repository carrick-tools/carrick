export interface Order {
  id: string;
  sku: string;
  quantity: number;
}

export interface CreateOrder {
  sku: string;
  quantity: number;
}

export interface NewItem {
  label: string;
}

export interface Page<T> {
  items: T[];
  next: string | null;
}

export interface TicketUpdate {
  ticketId: string;
  status: string;
}

export interface Parcel {
  id: string;
  weightGrams: number;
}
