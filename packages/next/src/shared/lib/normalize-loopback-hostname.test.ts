import { normalizeLoopbackHostname } from './normalize-loopback-hostname'

describe('normalizeLoopbackHostname', () => {
  it.each(['localhost', '127.0.0.1', '127.0.1.0', '[::1]'])(
    'normalizes %s to localhost',
    (hostname) => {
      expect(normalizeLoopbackHostname(hostname)).toBe('localhost')
    }
  )

  it('preserves non-loopback hostnames', () => {
    expect(normalizeLoopbackHostname('example.com')).toBe('example.com')
  })
})
