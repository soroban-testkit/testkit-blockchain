import { seedRemittances } from '../remittance.js';
import { seedRemittanceSchema } from '../../schemas/remittanceSchemas.js';

describe('Seed remittance validation', () => {
  it('should validate all seed records successfully', () => {
    for (const record of seedRemittances) {
      const parsed = seedRemittanceSchema.parse(record);
      expect(parsed).toEqual(record);
    }
  });

  it('should fail validation when required fields are missing', () => {
    const invalidRecord = { user_id: 'user_001', amount: 500 };
    expect(() => seedRemittanceSchema.parse(invalidRecord)).toThrow();
  });

  it('should fail validation when fields have wrong types', () => {
    const invalidRecord = {
      user_id: 'user_001',
      amount: '500',
      month: 'January',
      status: 'Completed',
    };
    expect(() => seedRemittanceSchema.parse(invalidRecord)).toThrow();
  });

  it('should fail validation when amount is not positive', () => {
    const invalidRecord = { user_id: 'user_001', amount: 0, month: 'January', status: 'Completed' };
    expect(() => seedRemittanceSchema.parse(invalidRecord)).toThrow();
  });

  it('should fail validation when month is empty', () => {
    const invalidRecord = { user_id: 'user_001', amount: 500, month: '', status: 'Completed' };
    expect(() => seedRemittanceSchema.parse(invalidRecord)).toThrow();
  });

  it('should fail validation when status is empty', () => {
    const invalidRecord = { user_id: 'user_001', amount: 500, month: 'January', status: '' };
    expect(() => seedRemittanceSchema.parse(invalidRecord)).toThrow();
  });
});
