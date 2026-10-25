import { ItemsClient, EventsClient } from "../../client/src/index";

export const items = new ItemsClient({ send: fetch });
export const events = new EventsClient();
