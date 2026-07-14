const restoreDescriptors: Array<() => void> = []

const overrideBooleanDescriptor = (
  target: NodeJS.WriteStream,
  property: 'isTTY',
  value: boolean | undefined
) => {
  const descriptor = Object.getOwnPropertyDescriptor(target, property)
  restoreDescriptors.push(() => {
    if (descriptor) {
      Object.defineProperty(target, property, descriptor)
    } else {
      delete (target as any)[property]
    }
  })
  Object.defineProperty(target, property, {
    configurable: true,
    enumerable: false,
    value,
    writable: true,
  })
}

describe('picocolors', () => {
  const originalEnv = { ...process.env }

  const restoreEnv = () => {
    for (const key of Object.keys(process.env)) {
      delete process.env[key]
    }
    Object.assign(process.env, originalEnv)
  }

  afterEach(() => {
    restoreEnv()
    while (restoreDescriptors.length > 0) {
      restoreDescriptors.pop()?.()
    }
    jest.resetModules()
  })

  it('disables colors when FORCE_COLOR is 0', () => {
    delete process.env.NO_COLOR
    delete process.env.CI
    process.env.FORCE_COLOR = '0'
    process.env.TERM = 'xterm-256color'
    overrideBooleanDescriptor(process.stdout, 'isTTY', true)

    const { red } = require('./picocolors') as typeof import('./picocolors')

    expect(red('error')).toBe('error')
  })

  it('enables colors when FORCE_COLOR is 1 without a TTY', () => {
    delete process.env.NO_COLOR
    delete process.env.CI
    process.env.FORCE_COLOR = '1'
    overrideBooleanDescriptor(process.stdout, 'isTTY', false)

    const { red } = require('./picocolors') as typeof import('./picocolors')

    expect(red('error')).toBe('\x1b[31merror\x1b[39m')
  })
})
