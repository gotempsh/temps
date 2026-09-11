import { expect, test } from 'bun:test'
import { formatTraceDuration } from './trace-presentation'
test('formats trace durations without noisy millisecond decimals', () => {
  expect(formatTraceDuration(17839.72)).toBe('17.8 s')
  expect(formatTraceDuration(3.6)).toBe('4 ms')
  expect(formatTraceDuration(0.04)).toBe('40 µs')
  expect(formatTraceDuration(0)).toBe('0 ms')
  expect(formatTraceDuration(0.0001)).toBe('<1 µs')
  expect(formatTraceDuration(NaN)).toBe('—')
  expect(formatTraceDuration(-1)).toBe('—')
})
