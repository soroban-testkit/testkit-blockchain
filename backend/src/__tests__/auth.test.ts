import { jest, describe, it, expect, beforeAll, beforeEach } from '@jest/globals';
import { Keypair } from '@stellar/stellar-sdk';

// Auth challenge nonces are persisted through cacheService (Redis). Tests
// run without a real Redis instance, so we back it with an in-memory Map
// that honors the same single-use, fenced compare-and-delete contract as
// the real `deleteIfMatch` Lua script (see cacheService.ts) — this is what
// lets the nonce-replay regression tests below exercise the real control
// flow.
const nonceStore = new Map<string, unknown>();

const mockSet = jest
  .fn<(key: string, value: unknown, ttlSeconds?: number) => Promise<void>>()
  .mockImplementation(async (key, value) => {
    nonceStore.set(key, value);
  });
const mockDeleteIfMatch = jest
  .fn<(key: string, expectedValue: string) => Promise<boolean>>()
  .mockImplementation(async (key, expectedValue) => {
    if (!nonceStore.has(key)) return false;
    if (nonceStore.get(key) !== expectedValue) return false;
    nonceStore.delete(key);
    return true;
  });
const mockGet = jest.fn<(key: string) => Promise<null>>().mockResolvedValue(null);
const mockDelete = jest.fn<(key: string) => Promise<void>>().mockResolvedValue(undefined);

jest.unstable_mockModule('../services/cacheService.js', () => ({
  cacheService: {
    set: mockSet,
    get: mockGet,
    deleteIfMatch: mockDeleteIfMatch,
    delete: mockDelete,
    setNotExists: jest.fn<() => Promise<boolean>>().mockResolvedValue(true),
    ping: jest.fn<() => Promise<string>>().mockResolvedValue('ok'),
    invalidatePattern: jest.fn<() => Promise<void>>().mockResolvedValue(undefined),
  },
}));

const request = (await import('supertest')).default;
const { default: app } = await import('../app.js');

describe('Auth API', () => {
  beforeAll(() => {
    process.env.JWT_SECRET = 'test-secret-key-for-jest';
  });

  beforeEach(() => {
    nonceStore.clear();
  });

  describe('POST /api/auth/challenge', () => {
    it('should generate a challenge for a valid public key', async () => {
      const keypair = Keypair.random();

      const response = await request(app)
        .post('/api/auth/challenge')
        .send({ publicKey: keypair.publicKey() })
        .expect(200);

      expect(response.body.success).toBe(true);
      expect(response.body.data.message).toContain('Sign this message');
      expect(response.body.data.nonce).toBeDefined();
      expect(response.body.data.timestamp).toBeDefined();
      expect(response.body.data.expiresIn).toBe(5 * 60 * 1000);
    });

    it('should reject invalid public key', async () => {
      const response = await request(app)
        .post('/api/auth/challenge')
        .send({ publicKey: 'invalid-key' })
        .expect(400);

      expect(response.body.success).toBe(false);
    });

    it('should reject missing public key', async () => {
      const response = await request(app).post('/api/auth/challenge').send({}).expect(400);

      expect(response.body.success).toBe(false);
    });
  });

  describe('POST /api/auth/login', () => {
    it('should login with valid signature', async () => {
      const keypair = Keypair.random();

      const challengeResponse = await request(app)
        .post('/api/auth/challenge')
        .send({ publicKey: keypair.publicKey() })
        .expect(200);

      const message = challengeResponse.body.data.message;
      const signature = keypair.sign(Buffer.from(message, 'utf-8')).toString('base64');

      const loginResponse = await request(app)
        .post('/api/auth/login')
        .send({
          publicKey: keypair.publicKey(),
          message,
          signature,
        })
        .expect(200);

      expect(loginResponse.body.success).toBe(true);
      expect(loginResponse.body.data.token).toBeDefined();
      expect(loginResponse.body.data.publicKey).toBe(keypair.publicKey());
    });

    it('should reject invalid signature', async () => {
      const keypair = Keypair.random();
      const differentKeypair = Keypair.random();

      const challengeResponse = await request(app)
        .post('/api/auth/challenge')
        .send({ publicKey: keypair.publicKey() })
        .expect(200);

      const message = challengeResponse.body.data.message;
      const wrongSignature = differentKeypair
        .sign(Buffer.from(message, 'utf-8'))
        .toString('base64');

      const loginResponse = await request(app)
        .post('/api/auth/login')
        .send({
          publicKey: keypair.publicKey(),
          message,
          signature: wrongSignature,
        })
        .expect(401);

      expect(loginResponse.body.success).toBe(false);
    });

    it('should reject missing fields', async () => {
      const response = await request(app).post('/api/auth/login').send({}).expect(400);

      expect(response.body.success).toBe(false);
    });
  });

  describe('GET /api/auth/verify', () => {
    it('should verify valid token', async () => {
      const keypair = Keypair.random();

      const challengeResponse = await request(app)
        .post('/api/auth/challenge')
        .send({ publicKey: keypair.publicKey() })
        .expect(200);

      const message = challengeResponse.body.data.message;
      const signature = keypair.sign(Buffer.from(message, 'utf-8')).toString('base64');

      const loginResponse = await request(app)
        .post('/api/auth/login')
        .send({
          publicKey: keypair.publicKey(),
          message,
          signature,
        })
        .expect(200);

      const token = loginResponse.body.data.token;

      const verifyResponse = await request(app)
        .get('/api/auth/verify')
        .set('Authorization', `Bearer ${token}`)
        .expect(200);

      expect(verifyResponse.body.success).toBe(true);
      expect(verifyResponse.body.data.valid).toBe(true);
      expect(verifyResponse.body.data.publicKey).toBe(keypair.publicKey());
      expect(verifyResponse.body.data.role).toBe('borrower');
      expect(Array.isArray(verifyResponse.body.data.scopes)).toBe(true);
      expect(verifyResponse.body.data.scopes).toContain('read:loans');
    });

    it('should reject missing token', async () => {
      const response = await request(app).get('/api/auth/verify').expect(401);

      expect(response.body.success).toBe(false);
    });

    it('should reject invalid token', async () => {
      const response = await request(app)
        .get('/api/auth/verify')
        .set('Authorization', 'Bearer invalid-token')
        .expect(401);

      expect(response.body.success).toBe(false);
    });
  });

  describe('Rate limiting', () => {
    it('should return 429 after 10 challenge requests from same IP', async () => {
      const keypair = Keypair.random();
      let lastResponse: {
        status: number;
        body: { success: boolean };
        headers: Record<string, string>;
      } = undefined as unknown as {
        status: number;
        body: { success: boolean };
        headers: Record<string, string>;
      };
      for (let i = 0; i < 11; i++) {
        lastResponse = await request(app)
          .post('/api/auth/challenge')
          .set('X-Forwarded-For', '1.2.3.4')
          .send({ publicKey: keypair.publicKey() });
      }
      expect(lastResponse.status).toBe(429);
      expect(lastResponse.body.success).toBe(false);
    });

    it('should return 429 and Retry-After after 5 login attempts from same IP', async () => {
      const keypair = Keypair.random();
      let lastResponse: {
        status: number;
        body: { success: boolean };
        headers: Record<string, string>;
      } = undefined as unknown as {
        status: number;
        body: { success: boolean };
        headers: Record<string, string>;
      };
      for (let i = 0; i < 6; i++) {
        lastResponse = await request(app)
          .post('/api/auth/login')
          .set('X-Forwarded-For', '5.6.7.8')
          .send({
            publicKey: keypair.publicKey(),
            message: 'fake-message',
            signature: 'fake-signature',
          });
      }
      expect(lastResponse.status).toBe(429);
      expect(lastResponse.headers['retry-after']).toBeDefined();
    });

    it('should return 429 after 5 login attempts with same public key', async () => {
      const keypair = Keypair.random();
      let lastResponse: {
        status: number;
        body: { success: boolean };
        headers: Record<string, string>;
      } = undefined as unknown as {
        status: number;
        body: { success: boolean };
        headers: Record<string, string>;
      };
      for (let i = 0; i < 6; i++) {
        lastResponse = await request(app)
          .post('/api/auth/login')
          .set('X-Forwarded-For', `9.9.9.${i}`)
          .send({
            publicKey: keypair.publicKey(),
            message: 'fake-message',
            signature: 'fake-signature',
          });
      }
      expect(lastResponse.status).toBe(429);
      expect(lastResponse.body.success).toBe(false);
    });
  });
});

describe('authService unit tests', () => {
  let authService: typeof import('../services/authService.js');

  beforeAll(async () => {
    authService = await import('../services/authService.js');
  });

  describe('verifySignature', () => {
    it('should return true for valid signature', () => {
      const keypair = Keypair.random();
      const message = 'test message';
      const signature = keypair.sign(Buffer.from(message, 'utf-8')).toString('base64');

      const result = authService.verifySignature(keypair.publicKey(), message, signature);
      expect(result).toBe(true);
    });

    it('should return false for wrong signer', () => {
      const keypair1 = Keypair.random();
      const keypair2 = Keypair.random();
      const message = 'test message';
      const signature = keypair1.sign(Buffer.from(message, 'utf-8')).toString('base64');

      const result = authService.verifySignature(keypair2.publicKey(), message, signature);
      expect(result).toBe(false);
    });

    it('should return false for non-64-byte signature', () => {
      const keypair = Keypair.random();
      const message = 'test message';
      const invalidSignature = Buffer.from('short').toString('base64');

      const result = authService.verifySignature(keypair.publicKey(), message, invalidSignature);
      expect(result).toBe(false);
    });

    it('should return false for non-base64 input', () => {
      const keypair = Keypair.random();
      const message = 'test message';
      const invalidSignature = '!!!not-base64!!!';

      const result = authService.verifySignature(keypair.publicKey(), message, invalidSignature);
      expect(result).toBe(false);
    });

    it('should return false for invalid public key', () => {
      const message = 'test message';
      const signature = Buffer.from('a'.repeat(64)).toString('base64');

      const result = authService.verifySignature('INVALID_KEY', message, signature);
      expect(result).toBe(false);
    });
  });

  describe('verifyChallengeTimestamp', () => {
    it('should accept timestamp at the window edge', () => {
      const maxAge = 5 * 60 * 1000; // 5 minutes
      const timestamp = Date.now() - maxAge;

      const result = authService.verifyChallengeTimestamp(timestamp, maxAge);
      expect(result).toBe(true);
    });

    it('should accept timestamp under the window', () => {
      const maxAge = 5 * 60 * 1000;
      const timestamp = Date.now() - 1000; // 1 second ago

      const result = authService.verifyChallengeTimestamp(timestamp, maxAge);
      expect(result).toBe(true);
    });

    it('should reject timestamp over the window', () => {
      const maxAge = 5 * 60 * 1000;
      const timestamp = Date.now() - maxAge - 1000; // 1 second too old

      const result = authService.verifyChallengeTimestamp(timestamp, maxAge);
      expect(result).toBe(false);
    });

    it('should accept future timestamp within tolerance', () => {
      const maxAge = 5 * 60 * 1000;
      const timestamp = Date.now() + 1000; // 1 second in future

      const result = authService.verifyChallengeTimestamp(timestamp, maxAge);
      expect(result).toBe(true);
    });
  });

  describe('extractBearerToken', () => {
    it('should extract token from valid Bearer header', () => {
      const token = 'my-jwt-token';
      const header = `Bearer ${token}`;

      const result = authService.extractBearerToken(header);
      expect(result).toBe(token);
    });

    it('should return null for undefined header', () => {
      const result = authService.extractBearerToken(undefined);
      expect(result).toBeNull();
    });

    it('should return null for wrong scheme', () => {
      const result = authService.extractBearerToken('Basic dGVzdA==');
      expect(result).toBeNull();
    });

    it('should return null for malformed Bearer with no token', () => {
      const result = authService.extractBearerToken('Bearer');
      expect(result).toBeNull();
    });

    it('should return null for lowercase bearer', () => {
      const result = authService.extractBearerToken('bearer my-token');
      expect(result).toBeNull();
    });

    it('should return null for wrong part count', () => {
      const result = authService.extractBearerToken('Bearer token extra');
      expect(result).toBeNull();
    });
  });

  describe('challenge nonce single-use enforcement (#1068)', () => {
    it('consumes an issued nonce exactly once: a valid login succeeds, a replay is rejected', async () => {
      const keypair = Keypair.random();
      const nonce = 'a'.repeat(64);

      await authService.storeChallengeNonce(nonce, keypair.publicKey());

      // First consumption for the exact public key it was issued to succeeds.
      const firstConsume = await authService.consumeChallengeNonce(nonce, keypair.publicKey());
      expect(firstConsume).toBe(true);

      // Replaying the same nonce must fail: it has already been consumed.
      const secondConsume = await authService.consumeChallengeNonce(nonce, keypair.publicKey());
      expect(secondConsume).toBe(false);
    });

    it('rejects a nonce that was never issued (unknown nonce)', async () => {
      const keypair = Keypair.random();
      const result = await authService.consumeChallengeNonce(
        'never-issued-nonce',
        keypair.publicKey(),
      );
      expect(result).toBe(false);
    });

    it('rejects a nonce issued for a different public key', async () => {
      const keypair = Keypair.random();
      const otherKeypair = Keypair.random();
      const nonce = 'b'.repeat(64);

      await authService.storeChallengeNonce(nonce, keypair.publicKey());

      const result = await authService.consumeChallengeNonce(nonce, otherKeypair.publicKey());
      expect(result).toBe(false);

      // The nonce is left untouched for a mismatched public key, so the
      // rightful owner can still use it.
      const rightfulConsume = await authService.consumeChallengeNonce(nonce, keypair.publicKey());
      expect(rightfulConsume).toBe(true);
    });

    it('rejects a nonce that has expired server-side (evicted from the store)', async () => {
      const keypair = Keypair.random();
      const nonce = 'c'.repeat(64);

      await authService.storeChallengeNonce(nonce, keypair.publicKey());
      // Simulate TTL expiry: the store no longer has the nonce.
      nonceStore.delete(`auth:nonce:${nonce}`);

      const result = await authService.consumeChallengeNonce(nonce, keypair.publicKey());
      expect(result).toBe(false);
    });
  });
});
