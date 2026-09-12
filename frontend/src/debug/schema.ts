import { captureSchemas } from './capture-schemas';

type Schema = boolean | { [key: string]: unknown };
const schemas = captureSchemas as Schema[];
const map = (value: unknown): Record<string, unknown> => value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};

function equal(left: unknown, right: unknown): boolean {
  if (left === right) return true;
  if (!left || !right || typeof left !== 'object' || typeof right !== 'object') return false;
  if (Array.isArray(left) !== Array.isArray(right)) return false;
  const a = left as Record<string, unknown>, b = right as Record<string, unknown>;
  return Object.keys(a).length === Object.keys(b).length && Object.keys(a).every(key => Object.hasOwn(b, key) && equal(a[key], b[key]));
}

// Deliberately limited to the keywords in the generated schemas. The generator fails on
// new keywords; references resolve only to this bundle, never to a network destination.
function valid(value: unknown, schema: Schema, root: Schema): boolean {
  if (typeof schema === 'boolean') return schema;
  if (typeof schema.$ref === 'string') {
    const [id, fragment] = schema.$ref.split('#');
    const document = id ? schemas.find(item => map(item).$id === id) : root;
    if (!document) return false;
    let resolved: unknown = document;
    for (const part of fragment?.split('/').slice(1) ?? []) resolved = map(resolved)[part.replace(/~1/g, '/').replace(/~0/g, '~')];
    if (resolved === undefined || !valid(value, resolved as Schema, document)) return false;
  }
  if (schema.const !== undefined && !equal(value, schema.const)) return false;
  if (Array.isArray(schema.enum) && !schema.enum.includes(value)) return false;
  if (Array.isArray(schema.oneOf) && schema.oneOf.filter(child => valid(value, child as Schema, root)).length !== 1) return false;
  if (Array.isArray(schema.anyOf) && !schema.anyOf.some(child => valid(value, child as Schema, root))) return false;
  if (schema.type) {
    const types = Array.isArray(schema.type) ? schema.type : [schema.type];
    const type = value === null ? 'null' : Array.isArray(value) ? 'array' : typeof value;
    if (!types.includes(type) && !(types.includes('integer') && Number.isSafeInteger(value))) return false;
  }
  if (typeof value === 'number') {
    if (!Number.isFinite(value) || (typeof schema.minimum === 'number' && value < schema.minimum) || (typeof schema.maximum === 'number' && value > schema.maximum)) return false;
  }
  if (typeof value === 'string') {
    if (typeof schema.minLength === 'number' && [...value].length < schema.minLength) return false;
    if (typeof schema.pattern === 'string' && !new RegExp(schema.pattern).test(value)) return false;
    if (schema.format === 'date-time' && !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:\d\d)$/.test(value)) return false;
    if (schema.format === 'date-time') {
      if (!Number.isFinite(Date.parse(value))) return false;
      // The canonical timestamp schema requires UTC. Reject normalized invalid dates.
      if (value.endsWith('Z') && new Date(value).toISOString().slice(0, 19) !== value.slice(0, 19)) return false;
    }
  }
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    const data = value as Record<string, unknown>;
    if (Array.isArray(schema.required) && schema.required.some(key => !Object.hasOwn(data, key as string))) return false;
    const properties = map(schema.properties);
    for (const [key, entry] of Object.entries(data)) {
      if (Object.hasOwn(properties, key)) {
        if (!valid(entry, properties[key] as Schema, root)) return false;
      } else if (schema.additionalProperties !== undefined && !valid(entry, schema.additionalProperties as Schema, root)) return false;
    }
  }
  return true;
}

export function validateCaptureLine(value: unknown) {
  if (!valid(value, schemas[0], schemas[0])) throw new Error('Capture schema is invalid or unsupported.');
}
