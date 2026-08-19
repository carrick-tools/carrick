import { StorageUplink } from "storage-uplink";

const uplink = new StorageUplink();

export async function store(key: string, body: string): Promise<void> {
  await uplink.put(key, body);
}
