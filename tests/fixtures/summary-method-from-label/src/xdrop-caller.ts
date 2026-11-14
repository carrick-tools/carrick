import { dropShelf } from "./lib/xdrop";

export const shelfDropped = (id: string) => dropShelf("/racks", id);
