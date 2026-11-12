// The type every case below asks about.
export interface Item {
  id: string;
}

// An interface field typed Item, in the declaring file.
export interface Holder {
  item: Item;
}
