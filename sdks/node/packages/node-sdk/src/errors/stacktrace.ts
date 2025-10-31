import type { StackFrame, Exception } from './types.js';

const STACKTRACE_LIMIT = 50;

interface ParsedStackLine {
  filename?: string;
  function?: string;
  lineno?: number;
  colno?: number;
}

export function parseStackTrace(error: Error): StackFrame[] {
  if (!error.stack) {
    return [];
  }

  const lines = error.stack.split('\n');
  const frames: StackFrame[] = [];

  for (const line of lines) {
    if (line.includes('at ')) {
      const parsed = parseStackLine(line);
      if (parsed) {
        frames.push(createStackFrame(parsed));
      }
    }

    if (frames.length >= STACKTRACE_LIMIT) {
      break;
    }
  }

  return frames.reverse();
}

function parseStackLine(line: string): ParsedStackLine | null {
  // Try eval pattern first since it's more specific
  const evalPattern = /^\s*at eval \(eval at (.*?) \((.*?):(\d+):(\d+)\), .*?\)$/;
  let match = line.match(evalPattern);

  if (match) {
    return {
      function: `eval at ${match[1]}`,
      filename: match[2],
      lineno: parseInt(match[3], 10),
      colno: parseInt(match[4], 10),
    };
  }

  const nodePattern = /^\s*at (?:(.*?) \()?(.*?):(\d+):(\d+)\)?$/;
  const nodePatternWithoutColumn = /^\s*at (?:(.*?) \()?(.*?):(\d+)\)?$/;

  match = line.match(nodePattern);

  if (match) {
    return {
      function: match[1] || '<anonymous>',
      filename: match[2],
      lineno: parseInt(match[3], 10),
      colno: parseInt(match[4], 10),
    };
  }

  match = line.match(nodePatternWithoutColumn);

  if (match) {
    return {
      function: match[1] || '<anonymous>',
      filename: match[2],
      lineno: parseInt(match[3], 10),
    };
  }

  return null;
}

function createStackFrame(parsed: ParsedStackLine): StackFrame {
  const frame: StackFrame = {
    filename: parsed.filename,
    function: parsed.function,
    lineno: parsed.lineno,
    colno: parsed.colno,
  };

  if (parsed.filename) {
    frame.abs_path = parsed.filename;

    const isNodeInternal = parsed.filename.includes('node_modules') ||
                           parsed.filename.startsWith('internal/') ||
                           parsed.filename.startsWith('node:');

    frame.in_app = !isNodeInternal;
  }

  return frame;
}

export function exceptionFromError(error: Error): Exception {
  const exception: Exception = {
    type: error.name || 'Error',
    value: error.message,
    stacktrace: {
      frames: error.stack ? parseStackTrace(error) : [],
    },
    mechanism: {
      type: 'generic',
      handled: true,
    },
  };

  return exception;
}

export function extractErrorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message || 'Unknown error';
  }

  if (typeof error === 'string') {
    return error;
  }

  if (error && typeof error === 'object' && 'message' in error) {
    return String(error.message);
  }

  if (error === null) {
    return 'null';
  }

  if (error === undefined) {
    return 'undefined';
  }

  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}

export function isError(error: unknown): error is Error {
  return error instanceof Error ||
         (error !== null &&
          typeof error === 'object' &&
          'message' in error &&
          'stack' in error);
}
