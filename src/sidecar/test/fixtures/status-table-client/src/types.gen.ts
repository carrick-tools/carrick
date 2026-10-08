export interface Item {
  id: string;
  name: string;
}

export interface Problem {
  code: string;
  detail: string;
}

export type GetItemResponses = {
  200: Item;
};

export type GetItemErrors = {
  404: Problem;
};

export type ListItemsResponses = {
  200: Array<Item>;
};

export type DeleteItemResponses = {
  204: void;
};

export type DeleteItemErrors = {
  404: Problem;
};

export type ItemCounts = {
  200: number;
  404: number;
};
