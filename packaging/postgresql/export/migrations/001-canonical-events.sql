-- Version 1: an append-only canonical record and its immutable JSON-derived identity.
CREATE TABLE :"schema_name".schema_version (
  version integer PRIMARY KEY CHECK (version >= 1),
  runtime_role text NOT NULL
);
INSERT INTO :"schema_name".schema_version (version, runtime_role) VALUES (1, :'runtime_role');

CREATE TABLE :"schema_name".canonical_events (
  canonical_record jsonb NOT NULL,
  environment text GENERATED ALWAYS AS (canonical_record #>> '{metadata,runtime,environment}') STORED NOT NULL,
  bridge_id text GENERATED ALWAYS AS (canonical_record #>> '{metadata,resource,bridge_id}') STORED NOT NULL,
  record_id text GENERATED ALWAYS AS (canonical_record #>> '{metadata,identity,record_id}') STORED NOT NULL,
  subrecord_id bigint GENERATED ALWAYS AS ((canonical_record #>> '{metadata,identity,subrecord_id}')::bigint) STORED,
  resource jsonb GENERATED ALWAYS AS (canonical_record #> '{metadata,resource}') STORED NOT NULL,
  source_time text GENERATED ALWAYS AS (canonical_record #>> '{metadata,source_time}') STORED,
  observed_at text GENERATED ALWAYS AS (canonical_record #>> '{metadata,observed_at}') STORED NOT NULL,
  event_type text GENERATED ALWAYS AS (canonical_record #>> '{payload,kind}') STORED NOT NULL,
  CONSTRAINT canonical_events_record_shape_check CHECK (
    jsonb_typeof(canonical_record) = 'object'
    AND jsonb_typeof(canonical_record -> 'metadata') = 'object'
    AND jsonb_typeof(canonical_record #> '{metadata,identity}') = 'object'
    AND jsonb_typeof(canonical_record #> '{metadata,runtime}') = 'object'
    AND jsonb_typeof(canonical_record #> '{metadata,resource}') = 'object'
    AND canonical_record #>> '{metadata,runtime,environment}' IS NOT NULL
    AND canonical_record #>> '{metadata,resource,bridge_id}' IS NOT NULL
    AND canonical_record #>> '{metadata,identity,record_id}' IS NOT NULL
    AND canonical_record #>> '{metadata,observed_at}' IS NOT NULL
    AND jsonb_typeof(canonical_record -> 'payload') = 'object'
    AND canonical_record #>> '{payload,kind}' IN (
      'measurement', 'transaction_lifecycle', 'resource_status_change', 'point_change', 'command_result'
    )
  ),
  CONSTRAINT canonical_events_identity_key UNIQUE NULLS NOT DISTINCT (
    environment, bridge_id, record_id, subrecord_id
  )
);
