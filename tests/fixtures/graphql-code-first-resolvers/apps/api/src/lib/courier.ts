import { CourierClient } from '@example/courier-client';

const courier = new CourierClient({ region: 'eu-west' });

// A detected client, called when a request is handled: not a schema module.
export async function recallParcel(id: string): Promise<void> {
  await courier.recall({ parcel: id });
}
