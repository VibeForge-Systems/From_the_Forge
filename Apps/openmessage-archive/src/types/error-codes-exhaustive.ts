// Compile-time proof that ERROR_CODES in services/ipc.ts lists every
// ErrorCode the Rust side can send. `npm run typecheck` fails when a variant
// is added in src-tauri/src/error.rs and not listed there. Nothing imports
// this file; tsconfig's `include` makes vue-tsc check it.
import type { ErrorCode } from './generated/ErrorCode';
import type { ERROR_CODES } from '../services/ipc';

type Missing = Exclude<ErrorCode, (typeof ERROR_CODES)[number]>;
export const errorCodesAreExhaustive: [Missing] extends [never] ? true : never = true;
