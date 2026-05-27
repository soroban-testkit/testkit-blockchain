import { jest } from "@jest/globals";
import request from "supertest";

// ── Mocks ──────────────────────────────────────────────────────────────────
// Must be declared before any dynamic import of `../app.js`.

jest.unstable_mockModule("../db/connection.js", () => ({
  default: {
    query: jest
      .fn<() => Promise<any>>()
      .mockResolvedValue({ rows: [], rowCount: 0 }),
  },
  query: jest
    .fn<() => Promise<any>>()
    .mockResolvedValue({ rows: [], rowCount: 0 }),
  getClient: jest.fn(),
  withTransaction: jest.fn(),
}));

jest.unstable_mockModule("../services/cacheService.js", () => ({
  cacheService: {
    ping: jest.fn<() => Promise<string>>().mockResolvedValue("ok"),
  },
}));

jest.unstable_mockModule("../services/sorobanService.js", () => ({
  sorobanService: {
    ping: jest.fn<() => Promise<string>>().mockResolvedValue("ok"),
  },
}));

jest.unstable_mockModule("../services/notificationService.js", () => ({
  notificationService: {
    getNotifications: jest.fn().mockResolvedValue({
      notifications: [],
      total: 0,
      unreadCount: 0,
    }),
  },
}));

jest.unstable_mockModule("../services/eventStreamService.js", () => ({
  eventStreamService: {
    getStatus: jest.fn().mockReturnValue({
      totalConnections: 0,
      borrowerConnections: 0,
    }),
  },
}));

// Dynamic import so mocks are applied before the app boots.
const { default: app } = await import("../app.js");

// ── Tests ──────────────────────────────────────────────────────────────────

describe("API v1 route mounts for pool, notifications, and events", () => {
  it("GET /api/v1/pool/stats requires auth (returns 401, not 404)", async () => {
    const res = await request(app).get("/api/v1/pool/stats");
    // A 401 proves the route exists and auth middleware kicked in.
    // A 404 would mean the route was never mounted.
    expect(res.status).toBe(401);
  });

  it("GET /api/v1/notifications requires auth (returns 401, not 404)", async () => {
    const res = await request(app).get("/api/v1/notifications");
    expect(res.status).toBe(401);
  });

  it("GET /api/v1/events/status requires API key (not 404)", async () => {
    const res = await request(app).get("/api/v1/events/status");
    // 401 (bad key) or 500 (key not configured) both prove the route exists.
    // A 404 would mean the route was never mounted.
    expect(res.status).not.toBe(404);
  });
});
