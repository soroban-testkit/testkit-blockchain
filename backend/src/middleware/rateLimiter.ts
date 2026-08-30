import rateLimit, { ipKeyGenerator } from 'express-rate-limit';
import { RedisStore } from 'rate-limit-redis';
import { createClient } from 'redis';

const REDIS_URL = process.env.REDIS_URL || 'redis://localhost:6379';

// In non-production environments (tests, local dev without Redis) we use
// express-rate-limit's default in-memory store. This keeps every request
// functional even when no Redis server is reachable — otherwise the rate
// limiter's Redis-backed store rejects every request with "The client is
// closed" and the global error handler turns it into a 500 for ALL endpoints.
// In production we share state across instances via Redis.
const USE_REDIS_STORE = process.env.NODE_ENV === 'production';

let redisClient: ReturnType<typeof createClient> | undefined;
let redisReady = false;

function getRedisClient(): ReturnType<typeof createClient> | undefined {
  if (!USE_REDIS_STORE) return undefined;
  if (redisClient) return redisClient;
  const client = createClient({ url: REDIS_URL });
  // Absorb connection errors so a Redis outage degrades rate limiting instead
  // of throwing on the request path (express-rate-limit falls back to the
  // in-memory store when no `store` is provided).
  client.on('error', () => {});
  client.connect().catch(() => {});
  client.on('ready', () => {
    redisReady = true;
  });
  redisClient = client;
  return redisClient;
}

function redisStoreConfig(prefix: string): { store: RedisStore } | Record<string, never> {
  const client = getRedisClient();
  // Only hand out a Redis store once the connection is actually ready. Until
  // then return an empty config so the limiter uses its built-in memory store
  // and never 500s on the request path. Each limiter also gets its own store
  // with a unique prefix (express-rate-limit forbids sharing one instance).
  if (!client || !redisReady) return {};
  return {
    store: new RedisStore({
      prefix: `rl:${prefix}:`,
      sendCommand: (...args: string[]) => client.sendCommand(args),
    }),
  };
}

export const createRateLimiter = (name: string, max: number, windowMinutes: number = 15) =>
  rateLimit({
    windowMs: windowMinutes * 60 * 1000,
    max,
    ...redisStoreConfig(name),
    message: { error: 'Too many requests, please try again later.' },
    standardHeaders: true,
    legacyHeaders: false,
  });

export const globalRateLimiter = createRateLimiter('global', 100);
export const strictRateLimiter = createRateLimiter('strict', 10, 45);

// Auth endpoints: 10 req/min per IP (stricter rate limiting for brute-force protection)
export const challengeRateLimiter = rateLimit({
  windowMs: 60 * 1000, // 1 minute
  max: 10,
  ...redisStoreConfig('challenge'),
  keyGenerator: (req) => ipKeyGenerator(req.ip ?? 'unknown'),
  message: {
    success: false,
    message: 'Too many challenge requests, please try again later.',
  },
  standardHeaders: true,
  legacyHeaders: false,
  handler: (_req, res, _next, options) => {
    res.setHeader('Retry-After', Math.ceil(options.windowMs / 1000));
    res.status(429).json(options.message);
  },
});

export const loginRateLimiter = rateLimit({
  windowMs: 60 * 1000, // 1 minute
  max: 5,
  ...redisStoreConfig('login'),
  keyGenerator: (req) =>
    `${ipKeyGenerator(req.ip ?? 'unknown')}:${req.body?.publicKey ?? 'unknown'}`,
  message: {
    success: false,
    message: 'Too many login attempts, please try again later.',
  },
  standardHeaders: true,
  legacyHeaders: false,
  handler: (_req, res, _next, options) => {
    res.setHeader('Retry-After', Math.ceil(options.windowMs / 1000));
    res.status(429).json(options.message);
  },
});

export const ipLoginRateLimiter = rateLimit({
  windowMs: 60 * 1000, // 1 minute
  max: 5,
  ...redisStoreConfig('ipLogin'),
  keyGenerator: (req) => ipKeyGenerator(req.ip ?? 'unknown'),
  message: {
    success: false,
    message: 'Too many login attempts from this IP, please try again later.',
  },
  standardHeaders: true,
  legacyHeaders: false,
  handler: (_req, res, _next, options) => {
    res.setHeader('Retry-After', Math.ceil(options.windowMs / 1000));
    res.status(429).json(options.message);
  },
});

export const verifyRateLimiter = rateLimit({
  windowMs: 60 * 1000, // 1 minute
  max: 10,
  ...redisStoreConfig('verify'),
  keyGenerator: (req) => ipKeyGenerator(req.ip ?? 'unknown'),
  message: { success: false, message: 'Too many verification attempts' },
  standardHeaders: true,
  legacyHeaders: false,
  handler: (_req, res, _next, options) => {
    res.setHeader('Retry-After', Math.ceil(options.windowMs / 1000));
    res.status(429).json(options.message);
  },
});

// Simulation endpoints: 5 req/min per authenticated user
export const simulationRateLimiter = rateLimit({
  windowMs: 60 * 1000, // 1 minute
  max: 5,
  ...redisStoreConfig('simulation'),
  keyGenerator: (req) => {
    // Use authenticated user's public key if available, otherwise fall back to IP
    const user = (req as unknown as { user?: { publicKey: string } }).user;
    return user?.publicKey ?? ipKeyGenerator(req.ip ?? 'unknown');
  },
  message: {
    success: false,
    message: 'Too many simulation requests, please try again later.',
  },
  standardHeaders: true,
  legacyHeaders: false,
  skip: () => process.env.NODE_ENV === 'test',
  handler: (_req, res, _next, options) => {
    res.setHeader('Retry-After', Math.ceil(options.windowMs / 1000));
    res.status(429).json(options.message);
  },
});
