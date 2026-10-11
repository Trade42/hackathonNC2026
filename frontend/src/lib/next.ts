// Only allow same-site paths as a post-sign-in destination.
export function safeNext(raw: string | null, fallback = '/home') {
  if (!raw) return fallback
  try {
    const v = decodeURIComponent(raw)
    if (v.startsWith('/') && !v.startsWith('//') && !v.startsWith('/\\')) return v
  } catch {
    /* fall through */
  }
  return fallback
}
