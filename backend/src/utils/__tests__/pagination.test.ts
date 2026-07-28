import { parseQueryParams } from '../pagination.js';
import type { Request } from 'express';

describe('parseQueryParams amountRange', () => {
  const mockRequest = (amountRange: string | undefined): Partial<Request> => ({
    query: { amount_range: amountRange },
  });

  it('should leave a well-ordered min,max pair unchanged', () => {
    const req = mockRequest('10,100') as Request;
    expect(parseQueryParams(req).amountRange).toEqual({ min: 10, max: 100 });
  });

  it('should swap an out-of-order min,max pair', () => {
    const req = mockRequest('100,10') as Request;
    expect(parseQueryParams(req).amountRange).toEqual({ min: 10, max: 100 });
  });

  it('should return the same value for equal min and max', () => {
    const req = mockRequest('50,50') as Request;
    expect(parseQueryParams(req).amountRange).toEqual({ min: 50, max: 50 });
  });

  it('should return null when amount_range is not provided', () => {
    const req = mockRequest(undefined) as Request;
    expect(parseQueryParams(req).amountRange).toBeNull();
  });
});
