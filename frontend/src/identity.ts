export interface Identity {
  bridge_id: string;
  runtime: {
    environment: 'production' | 'staging' | 'demo';
    release_id: string;
    release_digest: string;
    process_instance_id: string;
  };
  selected_target_id: string | null;
}

export function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid response');
  return value as Record<string, unknown>;
}

export function boundedText(value: unknown, maximum = 256): string {
  if (typeof value !== 'string' || !value.length || value.length > maximum || /[\u0000-\u001f\u007f]/.test(value)) {
    throw new Error('Invalid response');
  }
  return value;
}

export function parseIdentity(value: unknown): Identity {
  const identity = object(value);
  const runtime = object(identity.runtime);
  const environment = runtime.environment;
  if (environment !== 'production' && environment !== 'staging' && environment !== 'demo') {
    throw new Error('Invalid identity');
  }
  return {
    bridge_id: boundedText(identity.bridge_id),
    runtime: {
      environment,
      release_id: boundedText(runtime.release_id),
      release_digest: boundedText(runtime.release_digest),
      process_instance_id: boundedText(runtime.process_instance_id),
    },
    selected_target_id: identity.selected_target_id == null ? null : boundedText(identity.selected_target_id),
  };
}

export function identityKey(identity: Identity): string {
  return JSON.stringify(identity);
}
