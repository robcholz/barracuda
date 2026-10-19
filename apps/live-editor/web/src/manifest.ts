/**
 * Checks a built manifest as the device checks it: against the app manager's own JSON schema
 * (crates/appkit/resources/app-manifest.json, read at build time) and the rules the schema cannot
 * say, so a manifest the device would reject fails the build in the browser first.
 */

import schema from "../../../../plugins/app-manager/crates/appkit/resources/app-manifest.json";

interface Schema {
  $ref?: string;
  type?: "object" | "array" | "string" | "integer";
  const?: unknown;
  enum?: unknown[];
  minLength?: number;
  maxLength?: number;
  pattern?: string;
  minimum?: number;
  maximum?: number;
  minItems?: number;
  maxItems?: number;
  items?: Schema;
  properties?: Record<string, Schema>;
  required?: string[];
  additionalProperties?: boolean;
}

const ROOT = schema as Schema & { $defs: Record<string, Schema> };

function resolve(node: Schema): Schema {
  return node.$ref ? ROOT.$defs[node.$ref.replace("#/$defs/", "")] : node;
}

/** The first way `value` breaks `node`, in words, at its JSON path. */
function check(value: unknown, node: Schema, path: string): string | null {
  const rule = resolve(node);
  if ("const" in rule && value !== rule.const)
    return `${path} must be ${JSON.stringify(rule.const)}`;
  if (rule.enum && !rule.enum.includes(value))
    return `${path} must be one of ${rule.enum.map((item) => JSON.stringify(item)).join(", ")}`;
  switch (rule.type) {
    case "string": {
      if (typeof value !== "string") return `${path} must be a string`;
      const length = [...value].length;
      if (rule.minLength !== undefined && length < rule.minLength)
        return `${path} must not be empty`;
      if (rule.maxLength !== undefined && length > rule.maxLength)
        return `${path} has at most ${rule.maxLength} characters`;
      if (rule.pattern && !new RegExp(rule.pattern).test(value))
        return `${path} may hold only letters, digits, ".", "_" and "-"`;
      break;
    }
    case "integer":
      if (typeof value !== "number" || !Number.isInteger(value))
        return `${path} must be an integer`;
      if (rule.minimum !== undefined && value < rule.minimum)
        return `${path} is at least ${rule.minimum}`;
      if (rule.maximum !== undefined && value > rule.maximum)
        return `${path} is at most ${rule.maximum}`;
      break;
    case "array": {
      if (!Array.isArray(value)) return `${path} must be an array`;
      if (rule.minItems !== undefined && value.length < rule.minItems)
        return `${path} has at least ${rule.minItems} items`;
      if (rule.maxItems !== undefined && value.length > rule.maxItems)
        return `${path} has at most ${rule.maxItems} items`;
      for (const [index, item] of value.entries()) {
        const problem =
          rule.items && check(item, rule.items, `${path}[${index}]`);
        if (problem) return problem;
      }
      break;
    }
    case "object": {
      if (!value || typeof value !== "object" || Array.isArray(value))
        return `${path} must be an object`;
      const record = value as Record<string, unknown>;
      for (const name of rule.required ?? [])
        if (!(name in record)) return `${path}.${name} is missing`;
      for (const [name, item] of Object.entries(record)) {
        const property = rule.properties?.[name];
        if (!property) {
          if (rule.additionalProperties === false)
            return `${path}.${name} is not a manifest field`;
          continue;
        }
        const problem = check(item, property, `${path}.${name}`);
        if (problem) return problem;
      }
      break;
    }
  }
  return null;
}

/** An app ID: 1–32 of a-z, 0-9 and -, as the app manager's `valid_id` takes it. */
export function validId(id: string): boolean {
  return /^[a-z0-9-]{1,32}$/.test(id);
}

/** Why the device would reject `manifest`, or `null` when it takes it. */
export function manifestProblem(manifest: unknown): string | null {
  const problem = check(manifest, ROOT, "$");
  if (problem) return problem;
  const {
    id,
    requires = [],
    uses = [],
    permissions = [],
  } = manifest as {
    id: string;
    requires?: string[];
    uses?: string[];
    permissions?: string[];
  };
  if (!validId(id)) return "$.id may hold only a-z, 0-9 and -";
  const named = [...requires, ...uses]
    .map((item) => item.split("@")[0])
    .concat(permissions);
  const twice = named.find((name, index) => named.indexOf(name) !== index);
  if (twice)
    return `${twice} is named more than once across requires, uses and permissions`;
  return null;
}
