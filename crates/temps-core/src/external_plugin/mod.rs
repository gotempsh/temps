// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! External plugin types shared between the platform and the plugin SDK.
//!
//! External plugins are standalone binaries that Temps discovers, spawns, and
//! communicates with over Unix domain sockets. This module contains the shared
//! types used by both the plugin SDK (`temps-plugin-sdk`) and the
//! plugin management crate (`temps-external-plugins`).
//!
//! The actual lifecycle management, proxying, and API handlers live in the
//! `temps-external-plugins` crate.

pub mod actor;
pub mod channel;
pub mod headers;
pub mod manifest;

pub use actor::{
    encode_plugin_actor_token, verify_plugin_actor_token, ActorPrincipal, PluginActorError,
    VerifiedActor, PLUGIN_ACTOR_TOKEN_TTL, PLUGIN_ACTOR_TOKEN_VERSION,
};
pub use channel::{
    ActorToken, ApiCall, ApiCallResult, CallOutcome, ChannelError, ChannelErrorCode, ChannelEvent,
    ChannelMessage, ChannelRequest, ChannelResponse, DeploymentInfo, EnvironmentInfo, HttpMethod,
    JsonBody, JsonBodyError, PlatformCall, PlatformCallRequest, PlatformCallResponse, ProjectInfo,
    ProtocolMismatch, VerifiedPluginApiCaller, PLUGIN_CHANNEL_PATH,
};
pub use manifest::{
    HandshakeMessage, NavEntry, NavSection, PluginCapability, PluginEvent, PluginHello,
    PluginLaunchConfig, PluginManifest, PluginManifestBuilder, PluginReady, UiManifest, UiRoute,
    EXTERNAL_PLUGIN_PROTOCOL_VERSION, PLUGIN_EVENTS_PATH,
};
