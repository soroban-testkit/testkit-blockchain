/**
 * Keyset pagination utilities for stable pagination under concurrent writes.
 *
 * This module provides encoding/decoding of opaque cursors and building
 * seek predicates for SQL queries. All cursors encode (created_at, seq) tuples
 * in base64url format to prevent client-side parsing.
 */

import { AppError } from '../errors/AppError.js';

/**
 * Decoded cursor representing a row's position in the keyset.
 */
export interface DecodedCursor {
    createdAt: Date;
    seq: bigint;
}

/**
 * Parameters for a keyset pagination query.
 */
export interface KeysetPaginationParams {
    snapshotSeq: bigint;
    cursor: string | null;
    limit: number;
}

const MAX_LIMIT = 100;
const DEFAULT_LIMIT = 50;

/**
 * Encodes a cursor as a base64url string.
 * The cursor format is: base64url(JSON.stringify({ createdAt: ISO8601, seq: string }))
 *
 * @param createdAt - The created_at timestamp of the row
 * @param seq - The seq value of the row
 * @returns Opaque base64url-encoded cursor string
 */
export function encodeCursor(createdAt: Date, seq: bigint): string {
    const payload = {
        createdAt: createdAt.toISOString(),
        seq: String(seq),
    };
    const json = JSON.stringify(payload);
    return base64urlEncode(json);
}

/**
 * Decodes a base64url cursor.
 *
 * @param cursor - Base64url-encoded cursor string
 * @returns Decoded cursor with createdAt and seq
 * @throws AppError with code INVALID_CURSOR if the cursor is malformed
 */
export function decodeCursor(cursor: string): DecodedCursor {
    try {
        const json = base64urlDecode(cursor);
        const payload = JSON.parse(json) as Record<string, unknown>;

        if (!payload.createdAt || typeof payload.createdAt !== 'string') {
            throw new Error('Missing or invalid createdAt');
        }

        if (!payload.seq || typeof payload.seq !== 'string') {
            throw new Error('Missing or invalid seq');
        }

        const createdAt = new Date(payload.createdAt);
        if (Number.isNaN(createdAt.getTime())) {
            throw new Error('Invalid createdAt date');
        }

        const seq = BigInt(payload.seq);

        return { createdAt, seq };
    } catch (error) {
        throw AppError.badRequest(`Invalid cursor: ${error instanceof Error ? error.message : 'unknown error'}`, {
            code: 'INVALID_CURSOR',
        });
    }
}

/**
 * Builds a keyset pagination WHERE clause for a SQL query.
 *
 * The clause ensures:
 * 1. Only rows within the snapshot are visible (seq <= snapshotSeq)
 * 2. Rows are ordered by (created_at DESC, seq DESC)
 * 3. The page window is seeked past the cursor
 *
 * @param cursor - Decoded cursor from the previous page, or null for the first page
 * @param snapshotSeq - The snapshot seq pinned at first request
 * @param columnPrefix - Optional prefix for column names (e.g. "t." for table alias)
 * @returns SQL WHERE clause fragment and parameter values
 *
 * @example
 * const { whereClause, params } = buildKeysetClause(cursor, snapshotSeq);
 * const query = `
 *   SELECT * FROM remittances
 *   WHERE ${whereClause}
 *   ORDER BY created_at DESC, seq DESC
 *   LIMIT $${params.length + 1}
 * `;
 * const fullParams = [...params, limit];
 */
export function buildKeysetClause(
    cursor: DecodedCursor | null,
    snapshotSeq: bigint,
    columnPrefix: string = '',
): {
    whereClause: string;
    params: (string | number | bigint)[];
} {
    const cols = (col: string) => (columnPrefix ? `${columnPrefix}.${col}` : col);
    const params: (string | number | bigint)[] = [];

    // Snapshot constraint: only rows up to the pinned seq are visible
    let whereClause = `${cols('seq')} <= $${params.length + 1}`;
    params.push(snapshotSeq);

    // Keyset seek constraint: rows strictly less than the cursor
    if (cursor) {
        // WHERE (created_at, seq) < (cursorCreatedAt, cursorSeq)
        // Expanded to: created_at < cursorCreatedAt OR (created_at = cursorCreatedAt AND seq < cursorSeq)
        whereClause += ` AND (${cols('created_at')} < $${params.length + 1} OR (${cols('created_at')} = $${params.length + 2} AND ${cols('seq')} < $${params.length + 3}))`;
        params.push(cursor.createdAt.toISOString());
        params.push(cursor.createdAt.toISOString());
        params.push(cursor.seq);
    }

    return { whereClause, params };
}

/**
 * Parses and validates keyset pagination query parameters.
 *
 * @param snapshotSeq - The snapshot_seq from query params (may be from first request or subsequent)
 * @param cursor - The cursor from query params (null for first page)
 * @param limit - The limit from query params
 * @returns Validated KeysetPaginationParams
 */
export function parseKeysetParams(
    snapshotSeq: string | number | null | undefined,
    cursor: string | null | undefined,
    limit: string | number | null | undefined,
): KeysetPaginationParams {
    // Parse snapshot_seq
    let parsedSnapshotSeq: bigint;
    if (snapshotSeq === null || snapshotSeq === undefined || snapshotSeq === '') {
        // First request; will be pinned by the handler
        parsedSnapshotSeq = BigInt(0);
    } else {
        try {
            parsedSnapshotSeq = BigInt(snapshotSeq);
        } catch {
            throw AppError.badRequest('Invalid snapshot_seq', { code: 'INVALID_SNAPSHOT_SEQ' });
        }
    }

    // Parse cursor
    const parsedCursor = cursor && typeof cursor === 'string' && cursor.trim().length > 0 ? cursor.trim() : null;

    // Parse limit
    let parsedLimit = DEFAULT_LIMIT;
    if (limit !== null && limit !== undefined && limit !== '') {
        const numLimit = typeof limit === 'number' ? limit : Number.parseInt(String(limit), 10);
        if (!Number.isFinite(numLimit) || numLimit < 1) {
            parsedLimit = DEFAULT_LIMIT;
        } else {
            parsedLimit = Math.min(numLimit, MAX_LIMIT);
        }
    }

    return {
        snapshotSeq: parsedSnapshotSeq,
        cursor: parsedCursor,
        limit: parsedLimit,
    };
}

/**
 * Base64url encoder (RFC 4648 section 5).
 */
function base64urlEncode(str: string): string {
    const buf = Buffer.from(str, 'utf-8');
    return buf.toString('base64').replace(/\+/g, '-').replace(/\//g, '_').replace(/=/g, '');
}

/**
 * Base64url decoder (RFC 4648 section 5).
 */
function base64urlDecode(str: string): string {
    // Add padding if needed
    const padded = str.padEnd(str.length + ((4 - (str.length % 4)) % 4), '=');
    const buf = Buffer.from(padded.replace(/-/g, '+').replace(/_/g, '/'), 'base64');
    return buf.toString('utf-8');
}
