// IPC types are generated from the Rust definitions by ts-rs (`cargo test`)
// into ./generated. Never edit those files by hand: change the Rust type and
// regenerate. CI fails if the committed copies are stale.
export type { AppInfo } from './generated/AppInfo';
export type { ArchiveLocationChoice } from './generated/ArchiveLocationChoice';
export type { ArchiveLocationStatus } from './generated/ArchiveLocationStatus';
export type { CheckLevel } from './generated/CheckLevel';
export type { ErrorCode } from './generated/ErrorCode';
export type { IpcError as IpcErrorPayload } from './generated/IpcError';
export type { LayoutPreferences } from './generated/LayoutPreferences';
export type { LocationCheck } from './generated/LocationCheck';
export type { LocationState } from './generated/LocationState';
export type { Settings } from './generated/Settings';
export type { SettingsPersistence } from './generated/SettingsPersistence';
export type { SettingsSnapshot } from './generated/SettingsSnapshot';
export type { SettingsUpdate } from './generated/SettingsUpdate';
export type { Theme } from './generated/Theme';
