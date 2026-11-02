// Delivery estimates join the schema when the server builds it.
export const etaTypeDefs = /* GraphQL */ `
  extend type Parcel {
    etaDays: Int
  }
`;
