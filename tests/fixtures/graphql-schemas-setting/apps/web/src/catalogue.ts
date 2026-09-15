import { gql, request } from 'graphql-request';

const WIDGETS = gql`
  query Widgets {
    widgets {
      id
      name
    }
  }
`;

export interface WidgetList {
  widgets: { id: string; name: string }[];
}

export async function loadCatalogue(): Promise<WidgetList> {
  return request<WidgetList>(`${process.env.WIDGETS_API_URL}/graphql`, WIDGETS);
}
