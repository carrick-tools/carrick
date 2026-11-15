import { listThings, getThing } from "@fixture/api";

export async function byFlag(archived: boolean) {
  return listThings(archived ? "archived" : "active");
}

const SITE = process.env.PREVIEW ? "preview" : "live";

export async function bySite() {
  return listThings(SITE);
}

export async function oneLiteral() {
  return listThings("archived");
}

export async function thingByFlag(draft: boolean) {
  return getThing(draft ? "draft" : "published");
}

export async function thingBySite() {
  return getThing(SITE);
}
