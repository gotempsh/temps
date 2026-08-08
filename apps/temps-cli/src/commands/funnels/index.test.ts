import { test, expect, describe } from 'bun:test'
import { parseStepsJson } from './index.js'

describe('parseStepsJson', () => {
  test('parses a valid array of steps', () => {
    const steps = parseStepsJson('[{"event_name":"page_view"},{"event_name":"signup"}]')
    expect(steps).toEqual([{ event_name: 'page_view' }, { event_name: 'signup' }])
  })

  test('rejects unparsable JSON instead of sending a broken request', () => {
    expect(parseStepsJson('not json')).toBeNull()
  })

  test('rejects a JSON object that is not an array', () => {
    // The most likely mistake: passing a single step object instead of an
    // array of steps. Sending it through would corrupt the funnel creation
    // request rather than failing loudly here.
    expect(parseStepsJson('{"event_name":"page_view"}')).toBeNull()
  })

  test('rejects steps missing an event_name', () => {
    expect(parseStepsJson('[{"foo":"bar"}]')).toBeNull()
  })

  test('rejects steps whose event_name is not a string', () => {
    expect(parseStepsJson('[{"event_name":123}]')).toBeNull()
  })

  test('accepts an empty array', () => {
    expect(parseStepsJson('[]')).toEqual([])
  })
})
