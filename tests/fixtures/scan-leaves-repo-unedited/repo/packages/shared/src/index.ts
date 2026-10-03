import type { Row } from 'generated-db-client';

export interface Shared {
  sharedId: string;
  row: Row;
}
