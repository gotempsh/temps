import { describe, it, expect } from 'vitest';
import { parseStackTrace, exceptionFromError, extractErrorMessage, isError } from '../stacktrace';

describe('stacktrace', () => {
  describe('parseStackTrace', () => {
    it('should parse a standard Node.js stack trace', () => {
      const error = new Error('Test error');
      error.stack = `Error: Test error
    at Object.<anonymous> (/app/src/index.js:42:15)
    at Module._compile (node:internal/modules/cjs/loader:1120:14)
    at Module._extensions..js (node:internal/modules/cjs/loader:1174:10)
    at /app/node_modules/some-package/index.js:10:5`;

      const frames = parseStackTrace(error);

      expect(frames).toHaveLength(4);

      // Frames are reversed, so last line becomes first frame
      expect(frames[0].filename).toBe('/app/node_modules/some-package/index.js');
      expect(frames[0].in_app).toBe(false);

      // Check one of the middle frames
      expect(frames[2].filename).toBe('node:internal/modules/cjs/loader');
      expect(frames[2].function).toBe('Module._compile');
      expect(frames[2].in_app).toBe(false);

      // First line in stack (last after reversal)
      expect(frames[3].filename).toBe('/app/src/index.js');
      expect(frames[3].function).toBe('Object.<anonymous>');
      expect(frames[3].in_app).toBe(true);
    });

    it('should handle stack traces without column numbers', () => {
      const error = new Error('Test');
      error.stack = `Error: Test
    at functionName (/path/to/file.js:100)
    at anotherFunction (/path/to/other.js:50)`;

      const frames = parseStackTrace(error);

      expect(frames).toHaveLength(2);
      // Frames are reversed
      expect(frames[0]).toMatchObject({
        filename: '/path/to/other.js',
        function: 'anotherFunction',
        lineno: 50,
        colno: undefined,
      });
      expect(frames[1]).toMatchObject({
        filename: '/path/to/file.js',
        function: 'functionName',
        lineno: 100,
        colno: undefined,
      });
    });

    it('should handle eval stack frames', () => {
      const error = new Error('Eval error');
      error.stack = `Error: Eval error
    at eval (eval at testFunction (/app/test.js:10:5), <anonymous>:1:1)`;

      const frames = parseStackTrace(error);

      expect(frames).toHaveLength(1);
      expect(frames[0].function).toBe('eval at testFunction');
      expect(frames[0].filename).toBe('/app/test.js');
      expect(frames[0].lineno).toBe(10);
      expect(frames[0].colno).toBe(5);
    });

    it('should handle anonymous functions', () => {
      const error = new Error('Anonymous');
      error.stack = `Error: Anonymous
    at /app/anonymous.js:5:10`;

      const frames = parseStackTrace(error);

      expect(frames[0]).toMatchObject({
        filename: '/app/anonymous.js',
        function: '<anonymous>',
        lineno: 5,
        colno: 10,
      });
    });

    it('should return empty array for error without stack', () => {
      const error = new Error('No stack');
      error.stack = undefined;

      const frames = parseStackTrace(error);
      expect(frames).toEqual([]);
    });

    it('should limit frames to 50', () => {
      const error = new Error('Many frames');
      const lines = ['Error: Many frames'];

      for (let i = 0; i < 100; i++) {
        lines.push(`    at function${i} (/file${i}.js:${i}:1)`);
      }

      error.stack = lines.join('\n');

      const frames = parseStackTrace(error);
      expect(frames).toHaveLength(50);
    });
  });

  describe('exceptionFromError', () => {
    it('should create exception object from Error', () => {
      const error = new TypeError('Cannot read property');
      error.stack = `TypeError: Cannot read property
    at testFunction (/app/test.js:10:5)`;

      const exception = exceptionFromError(error);

      expect(exception).toMatchObject({
        type: 'TypeError',
        value: 'Cannot read property',
        mechanism: {
          type: 'generic',
          handled: true,
        },
      });

      expect(exception.stacktrace?.frames).toHaveLength(1);
      expect(exception.stacktrace?.frames?.[0]).toMatchObject({
        filename: '/app/test.js',
        function: 'testFunction',
        lineno: 10,
        colno: 5,
      });
    });

    it('should handle Error without stack', () => {
      const error = new Error('No stack');
      error.stack = undefined;

      const exception = exceptionFromError(error);

      expect(exception).toMatchObject({
        type: 'Error',
        value: 'No stack',
        mechanism: {
          type: 'generic',
          handled: true,
        },
      });

      expect(exception.stacktrace?.frames).toEqual([]);
    });
  });

  describe('extractErrorMessage', () => {
    it('should extract message from Error instance', () => {
      const error = new Error('Test error message');
      expect(extractErrorMessage(error)).toBe('Test error message');
    });

    it('should handle Error with empty message', () => {
      const error = new Error('');
      expect(extractErrorMessage(error)).toBe('Unknown error');
    });

    it('should handle string errors', () => {
      expect(extractErrorMessage('String error')).toBe('String error');
    });

    it('should handle objects with message property', () => {
      const errorLike = { message: 'Custom message', code: 500 };
      expect(extractErrorMessage(errorLike)).toBe('Custom message');
    });

    it('should stringify objects without message', () => {
      const obj = { code: 404, status: 'Not Found' };
      expect(extractErrorMessage(obj)).toBe(JSON.stringify(obj));
    });

    it('should handle null and undefined', () => {
      expect(extractErrorMessage(null)).toBe('null');
      expect(extractErrorMessage(undefined)).toBe('undefined');
    });

    it('should handle circular references', () => {
      const circular: any = { prop: 'value' };
      circular.self = circular;

      const result = extractErrorMessage(circular);
      expect(result).toBe('[object Object]');
    });
  });

  describe('isError', () => {
    it('should identify Error instances', () => {
      expect(isError(new Error('test'))).toBe(true);
      expect(isError(new TypeError('test'))).toBe(true);
      expect(isError(new RangeError('test'))).toBe(true);
    });

    it('should identify error-like objects', () => {
      const errorLike = {
        message: 'Error message',
        stack: 'Error: Error message\n    at test.js:1:1',
      };
      expect(isError(errorLike)).toBe(true);
    });

    it('should reject non-errors', () => {
      expect(isError('string')).toBe(false);
      expect(isError(123)).toBe(false);
      expect(isError(null)).toBe(false);
      expect(isError(undefined)).toBe(false);
      expect(isError({})).toBe(false);
      expect(isError({ message: 'only message' })).toBe(false);
      expect(isError({ stack: 'only stack' })).toBe(false);
    });
  });
});
