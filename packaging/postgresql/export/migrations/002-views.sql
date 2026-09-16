-- Version 2: exact JSONB projections over the canonical append-only record.
SELECT set_config('search_path', format('%I, pg_catalog', :'schema_name'), true);
SELECT (version = 1) AS apply_v2 FROM schema_version
\gset

\if :apply_v2
  CREATE INDEX canonical_events_resource_idx ON :"schema_name".canonical_events USING gin (resource);
  CREATE INDEX canonical_events_source_time_idx ON :"schema_name".canonical_events (source_time);
  CREATE INDEX canonical_events_observed_at_idx ON :"schema_name".canonical_events (observed_at);
  CREATE INDEX canonical_events_event_type_idx ON :"schema_name".canonical_events (event_type);

  CREATE VIEW :"schema_name".measurements AS
    SELECT canonical_events.*,
           canonical_record #> '{payload,data}' AS payload,
           canonical_record #> '{payload,data,value}' AS value,
           canonical_record #> '{payload,data,quality}' AS quality,
           canonical_record #>> '{payload,data,measurement,original_unit}' AS unit,
           canonical_record #>> '{payload,data,source_time}' AS measurement_source_time,
           canonical_record #>> '{payload,data,observed_at}' AS measurement_observed_at
    FROM :"schema_name".canonical_events
    WHERE event_type = 'measurement';

  CREATE VIEW :"schema_name".transactions AS
    SELECT canonical_events.*,
           canonical_record #> '{payload,data}' AS payload
    FROM :"schema_name".canonical_events
    WHERE event_type = 'transaction_lifecycle';

  CREATE VIEW :"schema_name".command_results AS
    SELECT canonical_events.*,
           canonical_record #> '{payload,data}' AS payload
    FROM :"schema_name".canonical_events
    WHERE event_type = 'command_result';

  UPDATE schema_version SET version = 2;
\endif
