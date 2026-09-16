-- Keep the runtime role limited to append and read operations in this managed schema.
REVOKE ALL PRIVILEGES ON SCHEMA :"schema_name" FROM PUBLIC;
REVOKE ALL PRIVILEGES ON SCHEMA :"schema_name" FROM :"runtime_role";
GRANT USAGE ON SCHEMA :"schema_name" TO :"runtime_role";

REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA :"schema_name" FROM PUBLIC;
REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA :"schema_name" FROM :"runtime_role";
REVOKE ALL PRIVILEGES ON TABLE :"schema_name".schema_version FROM PUBLIC;
REVOKE ALL PRIVILEGES ON TABLE :"schema_name".schema_version FROM :"runtime_role";
REVOKE SELECT (version, runtime_role), INSERT (version, runtime_role), UPDATE (version, runtime_role), REFERENCES (version, runtime_role)
  ON TABLE :"schema_name".schema_version FROM :"runtime_role";

REVOKE SELECT (canonical_record, environment, bridge_id, record_id, subrecord_id, resource, source_time, observed_at, event_type),
  INSERT (canonical_record, environment, bridge_id, record_id, subrecord_id, resource, source_time, observed_at, event_type),
  UPDATE (canonical_record, environment, bridge_id, record_id, subrecord_id, resource, source_time, observed_at, event_type),
  REFERENCES (canonical_record, environment, bridge_id, record_id, subrecord_id, resource, source_time, observed_at, event_type)
  ON TABLE :"schema_name".canonical_events FROM :"runtime_role";
GRANT INSERT (canonical_record), SELECT ON :"schema_name".canonical_events TO :"runtime_role";

\if :target_v2
  REVOKE INSERT (canonical_record, environment, bridge_id, record_id, subrecord_id, resource, source_time, observed_at, event_type, payload, value, quality, unit, measurement_source_time, measurement_observed_at),
    UPDATE (canonical_record, environment, bridge_id, record_id, subrecord_id, resource, source_time, observed_at, event_type, payload, value, quality, unit, measurement_source_time, measurement_observed_at),
    REFERENCES (canonical_record, environment, bridge_id, record_id, subrecord_id, resource, source_time, observed_at, event_type, payload, value, quality, unit, measurement_source_time, measurement_observed_at)
    ON TABLE :"schema_name".measurements FROM :"runtime_role";
  REVOKE INSERT (canonical_record, environment, bridge_id, record_id, subrecord_id, resource, source_time, observed_at, event_type, payload),
    UPDATE (canonical_record, environment, bridge_id, record_id, subrecord_id, resource, source_time, observed_at, event_type, payload),
    REFERENCES (canonical_record, environment, bridge_id, record_id, subrecord_id, resource, source_time, observed_at, event_type, payload)
    ON TABLE :"schema_name".transactions, :"schema_name".command_results FROM :"runtime_role";
  GRANT SELECT ON TABLE :"schema_name".measurements, :"schema_name".transactions, :"schema_name".command_results TO :"runtime_role";
\endif
