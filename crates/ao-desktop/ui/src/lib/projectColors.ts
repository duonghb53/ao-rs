function fnv1a32(input: string): number {
  // Deterministic 32-bit hash (FNV-1a).
  let hash = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    hash ^= input.charCodeAt(i);
    hash = (hash * 0x01000193) >>> 0;
  }
  return hash >>> 0;
}

export function projectHue(projectId: string): number {
  // Hue is stable across reloads for the same projectId.
  return fnv1a32(projectId) % 360;
}

export function projectAccentStyle(projectId: string | null | undefined): Record<string, string> {
  if (!projectId) return {};
  return {
    // Used by CSS: `hsl(var(--project-h) ... / ...)`.
    "--project-h": String(projectHue(projectId)),
  };
}

