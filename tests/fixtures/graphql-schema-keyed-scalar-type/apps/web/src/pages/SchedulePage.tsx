import { gql, request } from '@example/gql-client';

interface ScheduleData {
  scheduleShipment: { id: string; scheduledDate: string };
}

const SCHEDULE = gql`
  mutation Schedule($id: ID!) {
    scheduleShipment(id: $id) {
      id
      scheduledDate
    }
  }
`;

export async function schedule(id: string): Promise<unknown> {
  const data = await request(SCHEDULE, { id });
  const res = await fetch(`${process.env.SHIPPING_API_URL}/health`);
  return { data, health: await res.json() };
}
