import { rules } from '@example/validation';

// A value from a package detection does not name: never admitted for GraphQL.
const retirementInput = rules.object({ id: rules.string() });

retirementInput.describe('retire a parcel');
