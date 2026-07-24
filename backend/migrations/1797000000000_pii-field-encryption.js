/**
 * PII Field Encryption Migration
 *
 * Adds encrypted PII infrastructure:
 * - pii_access_log table for decrypt audit trail
 * - Encrypted column types for future PII storage
 *
 * This migration does NOT modify existing tables with PII columns
 * since the current schema stores recipient data as Stellar addresses
 * (public keys), not plaintext PII. It establishes the infrastructure
 * for when PII fields are added.
 */

exports.up = async function (pgm) {
  // Create PII access log table for decrypt audit trail
  pgm.createTable('pii_access_log', {
    id: {
      type: 'uuid',
      primaryKey: true,
      default: pgm.func('gen_random_uuid()'),
    },
    actor: {
      type: 'text',
      notNull: true,
    },
    record_id: {
      type: 'text',
      notNull: true,
    },
    field: {
      type: 'text',
      notNull: true,
    },
    reason: {
      type: 'text',
      notNull: true,
    },
    request_id: {
      type: 'text',
      notNull: true,
    },
    created_at: {
      type: 'timestamptz',
      notNull: true,
      default: pgm.func('NOW()'),
    },
  });

  // Index for querying access logs by record
  pgm.createIndex('pii_access_log', ['record_id', 'created_at'], {
    name: 'idx_pii_access_log_record_created',
  });

  // Index for querying by actor (audit queries)
  pgm.createIndex('pii_access_log', ['actor', 'created_at'], {
    name: 'idx_pii_access_log_actor_created',
  });
};

exports.down = async function (pgm) {
  pgm.dropTable('pii_access_log');
};
