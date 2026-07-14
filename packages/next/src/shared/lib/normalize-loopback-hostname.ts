const REGEX_LOOPBACK_HOSTNAME =
  /^(?:127(?:\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)){3}|\[::1\]|localhost)$/

export function normalizeLoopbackHostname(hostname: string): string {
  return REGEX_LOOPBACK_HOSTNAME.test(hostname) ? 'localhost' : hostname
}
