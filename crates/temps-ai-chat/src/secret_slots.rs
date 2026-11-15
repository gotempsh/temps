// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Secret slots: credentials the user gives the assistant without the model
//! ever seeing them.
//!
//! The model asks for a slot (`request_secret`), naming the operation and the
//! request field the value is for. The user types the value into a form the
//! console renders; this service stores it encrypted. The model is handed only
//! the opaque handle `secret_ref:v1:<id>`, which the API-tool layer swaps for
//! the real value just before the approved request runs (see
//! `temps_ai_api_tools::SecretResolver`).
//!
//! # Guarantees
//!
//! * A slot belongs to one user, one conversation, one operation and one
//!   request field. Using it anywhere else is refused.
//! * Only the owner can fill, cancel or inspect a slot, and nobody can read a
//!   value back through this service's public surface: [`SecretSlotView`]
//!   carries no value, and the only decryption path is [`SlotResolver`].
//! * A slot expires [`SLOT_TTL`] after it was requested. Expired values are
//!   erased by [`SecretSlotService::sweep_expired`] and are unusable before
//!   that, because every use re-checks the expiry.
//! * A conversation can have only [`MAX_OPEN_SLOTS_PER_CONVERSATION`] open
//!   slots, so a looping model cannot fill the table.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
};
use serde::Serialize;
use temps_ai_api_tools::{
    SecretResolveError, SecretResolver, SecretUse, WriteApiToolsHandle, SECRET_REF_PREFIX,
};
use temps_auth::context::AuthContext;
use temps_core::secret_string::SecretString;
use temps_core::EncryptionService;
use temps_entities::ai_secret_slots;
use thiserror::Error;
use utoipa::ToSchema;

/// How long a slot stays usable after the model asked for it.
pub const SLOT_TTL: Duration = Duration::minutes(30);
/// Slots a conversation may have waiting for a value or holding one.
pub const MAX_OPEN_SLOTS_PER_CONVERSATION: u64 = 8;
/// Longest credential accepted. Private keys and service-account JSON are the
/// large cases; anything bigger is a mistake.
pub const MAX_SECRET_BYTES: usize = 16 * 1024;
const MAX_LABEL_CHARS: usize = 200;

pub const STATUS_AWAITING: &str = "awaiting";
pub const STATUS_FILLED: &str = "filled";
pub const STATUS_CANCELLED: &str = "cancelled";
pub const STATUS_EXPIRED: &str = "expired";

#[derive(Error, Debug)]
pub enum SecretSlotError {
    #[error("secret slot '{public_id}' not found")]
    NotFound { public_id: String },

    #[error("secret slot '{public_id}' is {status} and can no longer take a value")]
    NotOpen { public_id: String, status: String },

    #[error(
        "secret slot '{public_id}' expired at {expired_at}; ask the assistant to request it again"
    )]
    Expired {
        public_id: String,
        expired_at: DateTime<Utc>,
    },

    #[error(
        "conversation {conversation_id} already has {limit} open secret slots; \
         fill or cancel one before requesting another"
    )]
    TooManyOpen { conversation_id: i64, limit: u64 },

    #[error("invalid secret slot request: {message}")]
    Validation { message: String },

    #[error("failed to {operation} secret for slot '{public_id}': {reason}")]
    Encryption {
        operation: &'static str,
        public_id: String,
        reason: String,
    },

    #[error("database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

/// What a slot looks like to the console and to the model: everything except
/// the value.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SecretSlotView {
    /// The id inside the handle (`secret_ref:v1:<id>`).
    pub id: String,
    pub conversation_id: i64,
    pub operation_id: String,
    /// JSON pointer of the request field the value is for.
    pub field: String,
    /// What the form asks for.
    pub label: String,
    /// `awaiting`, `filled`, `cancelled` or `expired`.
    pub status: String,
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub filled_at: Option<DateTime<Utc>>,
}

impl SecretSlotView {
    /// The handle the model passes where the value belongs.
    pub fn handle(&self) -> String {
        format!("{SECRET_REF_PREFIX}{}", self.id)
    }
}

impl From<ai_secret_slots::Model> for SecretSlotView {
    fn from(model: ai_secret_slots::Model) -> Self {
        // A slot past its expiry reads as expired even before the sweep has
        // erased it, so the console never offers to fill a dead one.
        let status = if model.expires_at <= Utc::now()
            && matches!(model.status.as_str(), STATUS_AWAITING | STATUS_FILLED)
        {
            STATUS_EXPIRED.to_string()
        } else {
            model.status
        };
        Self {
            id: model.public_id,
            conversation_id: model.conversation_id,
            operation_id: model.operation_id,
            field: model.field,
            label: model.label,
            status,
            expires_at: model.expires_at,
            created_at: model.created_at,
            filled_at: model.filled_at,
        }
    }
}

#[derive(Clone)]
pub struct SecretSlotService {
    db: Arc<DatabaseConnection>,
    encryption: Arc<EncryptionService>,
}

impl SecretSlotService {
    pub fn new(db: Arc<DatabaseConnection>, encryption: Arc<EncryptionService>) -> Self {
        Self { db, encryption }
    }

    /// Open a slot for a value the user will provide.
    ///
    /// The caller has already checked that `field` is a credential field of
    /// `operation_id`; this method checks the rest: lengths and how many slots
    /// the conversation already has open.
    pub async fn request(
        &self,
        user_id: i32,
        conversation_id: i64,
        operation_id: &str,
        field: &str,
        label: &str,
    ) -> Result<SecretSlotView, SecretSlotError> {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
            return Err(SecretSlotError::Validation {
                message: format!(
                    "the label for {operation_id} {field} must be 1 to {MAX_LABEL_CHARS} characters"
                ),
            });
        }
        if operation_id.is_empty() || operation_id.len() > 160 || field.len() > 255 {
            return Err(SecretSlotError::Validation {
                message: "operation id or field is out of range".to_string(),
            });
        }

        // Asking again for the same field returns the slot already open for it.
        // The model's history keeps only the text of earlier turns, so a handle it
        // was given before the user filled the form is gone by the time the user
        // says "done"; re-requesting is how it finds the handle again and learns
        // whether the form was filled.
        if let Some(existing) = ai_secret_slots::Entity::find()
            .filter(ai_secret_slots::Column::ConversationId.eq(conversation_id))
            .filter(ai_secret_slots::Column::UserId.eq(user_id))
            .filter(ai_secret_slots::Column::OperationId.eq(operation_id))
            .filter(ai_secret_slots::Column::Field.eq(field))
            .filter(ai_secret_slots::Column::Status.is_in([STATUS_AWAITING, STATUS_FILLED]))
            .filter(ai_secret_slots::Column::ExpiresAt.gt(Utc::now()))
            .order_by_desc(ai_secret_slots::Column::CreatedAt)
            .one(self.db.as_ref())
            .await?
        {
            return Ok(existing.into());
        }

        let open = ai_secret_slots::Entity::find()
            .filter(ai_secret_slots::Column::ConversationId.eq(conversation_id))
            .filter(ai_secret_slots::Column::UserId.eq(user_id))
            .filter(ai_secret_slots::Column::Status.is_in([STATUS_AWAITING, STATUS_FILLED]))
            .filter(ai_secret_slots::Column::ExpiresAt.gt(Utc::now()))
            .count(self.db.as_ref())
            .await?;
        if open >= MAX_OPEN_SLOTS_PER_CONVERSATION {
            return Err(SecretSlotError::TooManyOpen {
                conversation_id,
                limit: MAX_OPEN_SLOTS_PER_CONVERSATION,
            });
        }

        let now = Utc::now();
        let model = ai_secret_slots::ActiveModel {
            public_id: Set(uuid::Uuid::new_v4().simple().to_string()),
            conversation_id: Set(conversation_id),
            user_id: Set(user_id),
            operation_id: Set(operation_id.to_string()),
            field: Set(field.to_string()),
            label: Set(label.to_string()),
            status: Set(STATUS_AWAITING.to_string()),
            ciphertext: Set(None),
            expires_at: Set(now + SLOT_TTL),
            created_at: Set(now),
            filled_at: Set(None),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        Ok(model.into())
    }

    /// Store the value the user typed. Allowed again while the slot is still
    /// open, so a mistyped key can be corrected before it is used.
    pub async fn fill(
        &self,
        user_id: i32,
        public_id: &str,
        value: &SecretString,
    ) -> Result<SecretSlotView, SecretSlotError> {
        if value.is_empty() {
            return Err(SecretSlotError::Validation {
                message: format!("secret slot '{public_id}' needs a non-empty value"),
            });
        }
        if value.expose().len() > MAX_SECRET_BYTES {
            return Err(SecretSlotError::Validation {
                message: format!(
                    "the value for secret slot '{public_id}' is larger than {MAX_SECRET_BYTES} bytes"
                ),
            });
        }
        let slot = self.owned(user_id, public_id).await?;
        self.require_open(&slot)?;

        let ciphertext = self
            .encryption
            .encrypt_string(value.expose())
            .map_err(|e| SecretSlotError::Encryption {
                operation: "encrypt",
                public_id: public_id.to_string(),
                reason: e.to_string(),
            })?;

        let updated = ai_secret_slots::ActiveModel {
            id: Set(slot.id),
            status: Set(STATUS_FILLED.to_string()),
            ciphertext: Set(Some(ciphertext)),
            filled_at: Set(Some(Utc::now())),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await?;
        Ok(updated.into())
    }

    /// Discard a slot and its value.
    pub async fn cancel(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<SecretSlotView, SecretSlotError> {
        let slot = self.owned(user_id, public_id).await?;
        let updated = ai_secret_slots::ActiveModel {
            id: Set(slot.id),
            status: Set(STATUS_CANCELLED.to_string()),
            ciphertext: Set(None),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await?;
        Ok(updated.into())
    }

    /// One slot, if it belongs to `user_id`. Never carries the value.
    pub async fn get_owned(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<SecretSlotView, SecretSlotError> {
        Ok(self.owned(user_id, public_id).await?.into())
    }

    /// The slots of one conversation that are still waiting for, or holding, a
    /// value.
    pub async fn list_open(
        &self,
        user_id: i32,
        conversation_id: i64,
    ) -> Result<Vec<SecretSlotView>, SecretSlotError> {
        let rows = ai_secret_slots::Entity::find()
            .filter(ai_secret_slots::Column::ConversationId.eq(conversation_id))
            .filter(ai_secret_slots::Column::UserId.eq(user_id))
            .filter(ai_secret_slots::Column::Status.is_in([STATUS_AWAITING, STATUS_FILLED]))
            .filter(ai_secret_slots::Column::ExpiresAt.gt(Utc::now()))
            .all(self.db.as_ref())
            .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Erase the values of expired slots. Returns how many were erased. Safe to
    /// run on an interval; the work is bounded by the number of expired rows,
    /// which the `expires_at` index finds directly.
    pub async fn sweep_expired(&self) -> Result<u64, SecretSlotError> {
        let result = ai_secret_slots::Entity::update_many()
            .col_expr(
                ai_secret_slots::Column::Ciphertext,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                ai_secret_slots::Column::Status,
                sea_orm::sea_query::Expr::value(STATUS_EXPIRED),
            )
            .filter(ai_secret_slots::Column::ExpiresAt.lte(Utc::now()))
            .filter(ai_secret_slots::Column::Status.is_in([STATUS_AWAITING, STATUS_FILLED]))
            .exec(self.db.as_ref())
            .await?;
        Ok(result.rows_affected)
    }

    /// A resolver for one user's one conversation. It is the only way to read
    /// a value back, and it refuses every slot that is not both theirs and the
    /// conversation's.
    pub fn resolver(&self, user_id: i32, conversation_id: i64) -> SlotResolver {
        SlotResolver {
            service: self.clone(),
            user_id,
            conversation_id,
        }
    }

    async fn owned(
        &self,
        user_id: i32,
        public_id: &str,
    ) -> Result<ai_secret_slots::Model, SecretSlotError> {
        // Someone else's slot is reported exactly like a missing one, so ids
        // cannot be probed.
        ai_secret_slots::Entity::find()
            .filter(ai_secret_slots::Column::PublicId.eq(public_id))
            .filter(ai_secret_slots::Column::UserId.eq(user_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SecretSlotError::NotFound {
                public_id: public_id.to_string(),
            })
    }

    fn require_open(&self, slot: &ai_secret_slots::Model) -> Result<(), SecretSlotError> {
        if slot.expires_at <= Utc::now() {
            return Err(SecretSlotError::Expired {
                public_id: slot.public_id.clone(),
                expired_at: slot.expires_at,
            });
        }
        if !matches!(slot.status.as_str(), STATUS_AWAITING | STATUS_FILLED) {
            return Err(SecretSlotError::NotOpen {
                public_id: slot.public_id.clone(),
                status: slot.status.clone(),
            });
        }
        Ok(())
    }
}

/// Resolves the handles one user's one conversation may use.
#[derive(Clone)]
pub struct SlotResolver {
    service: SecretSlotService,
    user_id: i32,
    conversation_id: i64,
}

impl std::fmt::Debug for SlotResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlotResolver")
            .field("user_id", &self.user_id)
            .field("conversation_id", &self.conversation_id)
            .finish()
    }
}

#[async_trait]
impl SecretResolver for SlotResolver {
    async fn resolve(
        &self,
        handle: &str,
        usage: &SecretUse<'_>,
    ) -> Result<SecretString, SecretResolveError> {
        let unknown = || SecretResolveError::Unknown {
            handle: handle.to_string(),
        };
        let public_id = handle.strip_prefix(SECRET_REF_PREFIX).ok_or_else(unknown)?;

        let slot = self
            .service
            .owned(self.user_id, public_id)
            .await
            .map_err(|e| match e {
                SecretSlotError::NotFound { .. } => unknown(),
                other => SecretResolveError::Storage {
                    handle: handle.to_string(),
                    reason: other.to_string(),
                },
            })?;

        // A slot from another conversation is not found, the same as a slot
        // that does not exist.
        if slot.conversation_id != self.conversation_id {
            return Err(unknown());
        }
        if slot.expires_at <= Utc::now() {
            return Err(SecretResolveError::Expired {
                handle: handle.to_string(),
            });
        }
        if slot.status != STATUS_FILLED {
            return Err(SecretResolveError::NotPermitted {
                handle: handle.to_string(),
                operation_id: usage.operation_id.to_string(),
                field: usage.field.clone(),
                reason: format!(
                    "the slot is {}; the user has not provided the value",
                    slot.status
                ),
            });
        }
        if slot.operation_id != usage.operation_id || slot.field != usage.field {
            return Err(SecretResolveError::NotPermitted {
                handle: handle.to_string(),
                operation_id: usage.operation_id.to_string(),
                field: usage.field.clone(),
                reason: format!("it was provided for {} {}", slot.operation_id, slot.field),
            });
        }

        let ciphertext = slot.ciphertext.ok_or_else(|| SecretResolveError::Storage {
            handle: handle.to_string(),
            reason: "the slot is marked filled but holds no value".to_string(),
        })?;
        let value = self
            .service
            .encryption
            .decrypt_string(&ciphertext)
            .map_err(|e| SecretResolveError::Storage {
                handle: handle.to_string(),
                reason: format!("decryption failed: {e}"),
            })?;
        Ok(SecretString::new(value))
    }
}

/// Name of the tool the model calls to ask the user for a credential.
pub const REQUEST_SECRET_TOOL: &str = "request_secret";

/// What the model is told about `request_secret`. It is the only place the
/// model learns that it must not ask for a credential in prose.
pub const REQUEST_SECRET_DESCRIPTION: &str = "Ask the user for the credentials an operation \
    needs (an access key, a password, a token) without seeing them yourself. NEVER ask the \
    user to type a credential into the chat, and never put one in a `temps_write` command: it \
    would be sent to the model provider and stored in the conversation. Instead call this tool \
    ONCE per operation with the `operation_id` you are about to run and EVERY credential field \
    it needs in `fields`, so the user fills one private form instead of several. Call it LAST: \
    only after you have settled every other value of the operation (infer what you can from the \
    conversation and the platform, ask for what is missing, and tell the user what you are about \
    to create), because the form interrupts the conversation. Give each \
    field a short `label` naming what it is in plain words (e.g. 'Secret access key') and, in \
    `purpose`, one sentence saying what the form is for. You get back one `handle` \
    (`secret_ref:v1:…`) per field. Pass each handle as the whole value of its field in your \
    `temps_write` command, e.g. `--secret_key secret_ref:v1:…`. Then tell the user to fill the \
    form and stop: the console sends you a message on its own when they save it, so do not ask \
    them to tell you. Propose the operation only after that message. You will not see \
    this tool's result again on later turns, so once the user says the form is filled, call \
    this tool again with the same operation and fields: it returns the same handles with status \
    `filled`. A handle works only for the operation and field it was requested for, only in this \
    conversation, and expires after 30 minutes. If `temps_write` reports a handle is unfilled or \
    expired, ask the user to fill the form again or request a new one. Use `credential_fields` \
    in the error to see which fields of an operation take a credential.";

/// Most credentials one form asks for. An operation has a handful at most; the
/// cap keeps a single call from using up the conversation's open slots.
const MAX_FIELDS_PER_REQUEST: usize = 6;

/// What the assistant's `request_secret` tool needs.
#[derive(Clone)]
pub struct SecretToolSupport {
    pub slots: SecretSlotService,
    pub write_api: Arc<WriteApiToolsHandle>,
}

/// JSON schema of the tool's arguments.
pub fn request_secret_parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "operation_id": {
                "type": "string",
                "description": "The write operation the credentials are for, e.g. `create_s3_source`."
            },
            "purpose": {
                "type": "string",
                "description": "One sentence for the user saying what the form is for, e.g. 'Register the backup bucket so schedules can store backups in it'."
            },
            "fields": {
                "type": "array",
                "description": "Every credential field of the operation you need from the user, in the order the form should show them.",
                "minItems": 1,
                "maxItems": MAX_FIELDS_PER_REQUEST,
                "items": {
                    "type": "object",
                    "properties": {
                        "field": {
                            "type": "string",
                            "description": "The request field that takes the credential, e.g. `secret_key` or `/secret_key`."
                        },
                        "label": {
                            "type": "string",
                            "description": "What the input is, in plain words, e.g. 'Secret access key'."
                        }
                    },
                    "required": ["field", "label"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["operation_id", "fields"],
        "additionalProperties": false
    })
}

/// One input of the form as the console renders it.
struct FormField {
    slot: SecretSlotView,
    /// The API's own description of the field, shown under the input.
    help: Option<String>,
}

/// The descriptor the model reads and the console turns into the form. It holds
/// handles, labels and field names, never a value. The list is called `requests`
/// rather than `secrets` because the transcript redactor masks any key whose name
/// contains "secret".
fn form_descriptor(
    operation_id: &str,
    operation_summary: Option<&str>,
    purpose: Option<&str>,
    fields: &[FormField],
) -> serde_json::Value {
    let all_filled = fields.iter().all(|f| f.slot.status == STATUS_FILLED);
    serde_json::json!({
        "status": if all_filled { "filled" } else { "awaiting_user" },
        "operation_id": operation_id,
        "operation_summary": operation_summary,
        "purpose": purpose,
        "requests": fields.iter().map(|f| serde_json::json!({
            "handle": f.slot.handle(),
            "field": f.slot.field,
            "label": f.slot.label,
            "help": f.help,
            "status": f.slot.status,
            "expires_at": f.slot.expires_at,
        })).collect::<Vec<_>>(),
        "next": if all_filled {
            "The user has already provided these values. Pass each `handle` as the whole \
             value of its field in your temps_write command."
        } else {
            "A private form is shown to the user. Tell them it is waiting and stop; the console \
             sends you a message when they save it, so do not ask them to confirm. Do not ask \
             for the values in chat. When that message arrives, call request_secret again with \
             the same operation and fields to get the handles back, and pass each as the whole \
             value of its field in your temps_write command."
        },
    })
}

impl SecretToolSupport {
    /// Handle one `request_secret` call and return the text the model reads
    /// back. The result names the slot and nothing else about it: there is no
    /// value yet, and there never will be one in this channel.
    pub async fn dispatch(
        &self,
        arguments: &str,
        conversation_id: i64,
        auth: &AuthContext,
    ) -> String {
        let args: serde_json::Value = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(error) => return format!("`request_secret` arguments are not valid JSON: {error}"),
        };
        let text = |value: &serde_json::Value, key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let Some(operation_id) = text(&args, "operation_id") else {
            return "`request_secret` needs a non-empty operation_id.".to_string();
        };
        let operation_id = operation_id.as_str();
        let purpose = text(&args, "purpose");

        // `fields` is the form; a single `field` + `label` is still accepted.
        let mut requested: Vec<(String, String)> = Vec::new();
        let listed = args.get("fields").and_then(serde_json::Value::as_array);
        for entry in listed.map(Vec::as_slice).unwrap_or_default() {
            let (Some(field), Some(label)) = (text(entry, "field"), text(entry, "label")) else {
                return "Every entry of `fields` needs a non-empty field and label.".to_string();
            };
            requested.push((field, label));
        }
        if let (Some(field), Some(label)) = (text(&args, "field"), text(&args, "label")) {
            requested.push((field, label));
        }
        if requested.is_empty() {
            return "`request_secret` needs `fields`: one entry per credential field, each with \
                    a field and a label."
                .to_string();
        }
        if requested.len() > MAX_FIELDS_PER_REQUEST {
            return format!(
                "`request_secret` takes at most {MAX_FIELDS_PER_REQUEST} fields per form, got {}.",
                requested.len()
            );
        }

        let Some(caller) = self.write_api.get() else {
            return "Secrets are not available yet (startup in progress).".to_string();
        };
        let Some(operation) = caller.describe(operation_id) else {
            return format!(
                "'{operation_id}' is not a write operation you can run, so no credential can be \
                 requested for it. Check the operation name with `temps_write --help`."
            );
        };

        // Validate the whole form before opening any slot, so a typo in the last
        // field does not leave the first ones open.
        let mut pointers: Vec<(String, String)> = Vec::new();
        for (field, label) in requested {
            let pointer = if field.starts_with('/') {
                field.clone()
            } else {
                format!("/{}", field.replace('~', "~0").replace('/', "~1"))
            };
            if !caller.is_credential_field(operation_id, &pointer) {
                let known = caller.credential_fields(operation_id);
                return if known.is_empty() {
                    format!(
                        "'{operation_id}' takes no credential fields, so there is nothing to \
                         request. If the user wants to give you a value for it, it is not a \
                         secret the API recognises; ask them how to proceed."
                    )
                } else {
                    format!(
                        "'{pointer}' is not a credential field of '{operation_id}'. Its \
                         credential fields are: {}.",
                        known.join(", ")
                    )
                };
            }
            if pointers.iter().any(|(existing, _)| *existing == pointer) {
                return format!("'{pointer}' is listed twice in `fields`; list each field once.");
            }
            pointers.push((pointer, label));
        }

        let mut form = Vec::with_capacity(pointers.len());
        for (pointer, label) in &pointers {
            match self
                .slots
                .request(
                    auth.user_id(),
                    conversation_id,
                    operation_id,
                    pointer,
                    label,
                )
                .await
            {
                Ok(slot) => {
                    let help = operation
                        .params
                        .iter()
                        .find(|p| format!("/{}", p.name) == *pointer)
                        .and_then(|p| p.description.clone());
                    form.push(FormField { slot, help });
                }
                Err(error @ SecretSlotError::TooManyOpen { .. }) => return error.to_string(),
                Err(error @ SecretSlotError::Validation { .. }) => return error.to_string(),
                Err(error) => {
                    tracing::error!(
                        conversation_id,
                        user_id = auth.user_id(),
                        operation_id,
                        error = %error,
                        "failed to open a secret slot"
                    );
                    return "A secret form could not be opened right now. Tell the user and try \
                            again shortly."
                        .to_string();
                }
            }
        }
        form_descriptor(
            operation_id,
            operation.summary.as_deref(),
            purpose.as_deref(),
            &form,
        )
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    const VALUE: &str = "wJalr-example-not-a-real-secret";

    fn encryption() -> Arc<EncryptionService> {
        Arc::new(EncryptionService::new("01234567890123456789012345678901").expect("test key"))
    }

    fn slot(status: &str, ciphertext: Option<String>) -> ai_secret_slots::Model {
        let now = Utc::now();
        ai_secret_slots::Model {
            id: 1,
            public_id: "0123456789abcdef0123456789abcdef".to_string(),
            conversation_id: 7,
            user_id: 3,
            operation_id: "create_s3_source".to_string(),
            field: "/secret_key".to_string(),
            label: "S3 secret access key".to_string(),
            status: status.to_string(),
            ciphertext,
            expires_at: now + Duration::minutes(10),
            created_at: now,
            filled_at: None,
        }
    }

    fn handle() -> String {
        format!("{SECRET_REF_PREFIX}0123456789abcdef0123456789abcdef")
    }

    fn usage<'a>(operation: &'a str, field: &str) -> SecretUse<'a> {
        SecretUse {
            operation_id: operation,
            field: field.to_string(),
        }
    }

    fn service_with(rows: Vec<Vec<ai_secret_slots::Model>>) -> SecretSlotService {
        let mut mock = MockDatabase::new(DatabaseBackend::Postgres);
        for result in rows {
            mock = mock.append_query_results(vec![result]);
        }
        SecretSlotService::new(Arc::new(mock.into_connection()), encryption())
    }

    fn filled_slot() -> ai_secret_slots::Model {
        let ciphertext = encryption().encrypt_string(VALUE).expect("encrypts");
        slot(STATUS_FILLED, Some(ciphertext))
    }

    #[tokio::test]
    async fn resolves_a_filled_slot_for_the_operation_and_field_it_was_provided_for() {
        let service = service_with(vec![vec![filled_slot()]]);
        let value = service
            .resolver(3, 7)
            .resolve(&handle(), &usage("create_s3_source", "/secret_key"))
            .await
            .expect("the owner may use it for the bound operation and field");
        assert_eq!(value.expose(), VALUE);
    }

    #[tokio::test]
    async fn refuses_the_same_handle_for_another_operation_or_field() {
        for (operation, field) in [
            ("delete_project", "/secret_key"),
            ("create_s3_source", "/access_key_id"),
        ] {
            let service = service_with(vec![vec![filled_slot()]]);
            let err = service
                .resolver(3, 7)
                .resolve(&handle(), &usage(operation, field))
                .await
                .expect_err("a slot is bound to one operation and field");
            assert!(
                matches!(err, SecretResolveError::NotPermitted { .. }),
                "{operation} {field}: {err:?}"
            );
            assert!(!err.to_string().contains(VALUE));
        }
    }

    #[tokio::test]
    async fn another_users_slot_reads_as_unknown() {
        // The ownership filter is part of the query, so another user's lookup
        // returns no row at all.
        let service = service_with(vec![vec![]]);
        let err = service
            .resolver(99, 7)
            .resolve(&handle(), &usage("create_s3_source", "/secret_key"))
            .await
            .expect_err("not theirs");
        assert!(matches!(err, SecretResolveError::Unknown { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn a_slot_from_another_conversation_reads_as_unknown() {
        let service = service_with(vec![vec![filled_slot()]]);
        let err = service
            .resolver(3, 8)
            .resolve(&handle(), &usage("create_s3_source", "/secret_key"))
            .await
            .expect_err("bound to conversation 7");
        assert!(matches!(err, SecretResolveError::Unknown { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn an_unfilled_cancelled_or_expired_slot_cannot_be_used() {
        let mut expired = filled_slot();
        expired.expires_at = Utc::now() - Duration::seconds(1);
        for (row, expect_expired) in [
            (slot(STATUS_AWAITING, None), false),
            (slot(STATUS_CANCELLED, None), false),
            (expired, true),
        ] {
            let service = service_with(vec![vec![row]]);
            let err = service
                .resolver(3, 7)
                .resolve(&handle(), &usage("create_s3_source", "/secret_key"))
                .await
                .expect_err("not usable");
            assert_eq!(
                matches!(err, SecretResolveError::Expired { .. }),
                expect_expired,
                "{err:?}"
            );
        }
    }

    #[tokio::test]
    async fn rejects_text_that_is_not_a_handle() {
        let service = service_with(vec![]);
        let err = service
            .resolver(3, 7)
            .resolve("hunter2", &usage("create_s3_source", "/secret_key"))
            .await
            .expect_err("not a handle");
        assert!(matches!(err, SecretResolveError::Unknown { .. }));
    }

    #[tokio::test]
    async fn fill_encrypts_the_value_and_the_view_never_carries_it() {
        let stored = filled_slot();
        // find (ownership), then the update's returning row.
        let service = service_with(vec![
            vec![slot(STATUS_AWAITING, None)],
            vec![stored.clone()],
        ]);
        let view = service
            .fill(3, &stored.public_id, &SecretString::new(VALUE))
            .await
            .expect("fills");
        assert_eq!(view.status, STATUS_FILLED);
        let rendered = format!(
            "{view:?} {}",
            serde_json::to_string(&view).expect("serializes")
        );
        assert!(!rendered.contains(VALUE), "{rendered}");
        assert_eq!(view.handle(), handle());
    }

    #[tokio::test]
    async fn fill_rejects_empty_and_oversized_values_before_touching_the_database() {
        let service = service_with(vec![]);
        let empty = service
            .fill(3, "abc", &SecretString::new(""))
            .await
            .expect_err("empty");
        assert!(matches!(empty, SecretSlotError::Validation { .. }));

        let huge = SecretString::new("x".repeat(MAX_SECRET_BYTES + 1));
        let oversized = service.fill(3, "abc", &huge).await.expect_err("too big");
        assert!(matches!(oversized, SecretSlotError::Validation { .. }));
    }

    #[tokio::test]
    async fn fill_refuses_an_expired_slot() {
        let mut expired = slot(STATUS_AWAITING, None);
        expired.expires_at = Utc::now() - Duration::seconds(1);
        let service = service_with(vec![vec![expired]]);
        let err = service
            .fill(
                3,
                "0123456789abcdef0123456789abcdef",
                &SecretString::new(VALUE),
            )
            .await
            .expect_err("expired");
        assert!(matches!(err, SecretSlotError::Expired { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn fill_of_someone_elses_slot_is_not_found() {
        let service = service_with(vec![vec![]]);
        let err = service
            .fill(
                99,
                "0123456789abcdef0123456789abcdef",
                &SecretString::new(VALUE),
            )
            .await
            .expect_err("not theirs");
        assert!(matches!(err, SecretSlotError::NotFound { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn asking_again_for_the_same_field_returns_the_open_slot() {
        // A filled slot is found by the first query, so no second slot is
        // inserted and the model gets the same handle back.
        let service = service_with(vec![vec![filled_slot()]]);
        let view = service
            .request(
                3,
                7,
                "create_s3_source",
                "/secret_key",
                "S3 secret access key",
            )
            .await
            .expect("re-request is answered from the open slot");
        assert_eq!(view.status, STATUS_FILLED);
        assert_eq!(view.handle(), handle());
    }

    #[tokio::test]
    async fn asking_again_while_the_form_is_unfilled_reports_it_as_awaiting() {
        let service = service_with(vec![vec![slot(STATUS_AWAITING, None)]]);
        let view = service
            .request(
                3,
                7,
                "create_s3_source",
                "/secret_key",
                "S3 secret access key",
            )
            .await
            .expect("re-request is answered from the open slot");
        assert_eq!(view.status, STATUS_AWAITING);
    }

    #[tokio::test]
    async fn request_rejects_a_blank_label_before_touching_the_database() {
        let service = service_with(vec![]);
        let err = service
            .request(3, 7, "create_s3_source", "/secret_key", "   ")
            .await
            .expect_err("blank label");
        assert!(matches!(err, SecretSlotError::Validation { .. }), "{err:?}");
    }

    #[test]
    fn an_expired_row_reads_as_expired_even_before_the_sweep() {
        let mut row = filled_slot();
        row.expires_at = Utc::now() - Duration::seconds(5);
        assert_eq!(SecretSlotView::from(row).status, STATUS_EXPIRED);
    }

    #[test]
    fn the_model_debug_output_hides_the_ciphertext() {
        let row = slot(STATUS_FILLED, Some("sealed-bytes-marker".to_string()));
        let rendered = format!("{row:?}");
        assert!(!rendered.contains("sealed-bytes-marker"), "{rendered}");
        assert!(rendered.contains("***"));
        let json = serde_json::to_string(&row).expect("serializes");
        assert!(!json.contains("sealed-bytes-marker"), "{json}");
    }

    fn form_field(field: &str, label: &str, status: &str, id: &str) -> FormField {
        let mut row = slot(status, None);
        row.public_id = id.to_string();
        row.field = field.to_string();
        row.label = label.to_string();
        FormField {
            slot: row.into(),
            help: Some("Help from the API".to_string()),
        }
    }

    #[test]
    fn one_form_lists_every_field_and_is_filled_only_when_all_are() {
        let fields = vec![
            form_field("/secret_key", "Secret access key", STATUS_FILLED, "aaaa"),
            form_field("/session_token", "Session token", STATUS_AWAITING, "bbbb"),
        ];
        let descriptor = form_descriptor(
            "create_s3_source",
            Some("Create a new S3 source"),
            Some("Register the bucket"),
            &fields,
        );
        assert_eq!(descriptor["status"], "awaiting_user");
        assert_eq!(descriptor["operation_summary"], "Create a new S3 source");
        assert_eq!(descriptor["purpose"], "Register the bucket");
        let requests = descriptor["requests"].as_array().expect("a list");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["handle"], format!("{SECRET_REF_PREFIX}aaaa"));
        assert_eq!(requests[0]["label"], "Secret access key");
        assert_eq!(requests[0]["help"], "Help from the API");
        assert_eq!(requests[1]["status"], STATUS_AWAITING);

        let done = vec![form_field(
            "/secret_key",
            "Secret access key",
            STATUS_FILLED,
            "aaaa",
        )];
        assert_eq!(
            form_descriptor("create_s3_source", None, None, &done)["status"],
            "filled"
        );
    }

    #[test]
    fn the_transcript_redactor_keeps_the_descriptor_the_form_needs() {
        // The persisted transcript runs tool results through the credential
        // redactor. The descriptor has no value, and the console form needs the
        // handles, fields and labels to render, so they must survive it. That
        // includes the list's own key, which is why it is not named "secrets".
        let fields = vec![
            form_field("/secret_key", "Secret access key", STATUS_AWAITING, "aaaa"),
            form_field("/session_token", "Session token", STATUS_AWAITING, "bbbb"),
        ];
        let descriptor =
            form_descriptor("create_s3_source", Some("Create"), Some("Why"), &fields).to_string();
        let redacted = crate::sensitive::redact_json_string(&descriptor);
        for expected in [
            format!("{SECRET_REF_PREFIX}aaaa"),
            format!("{SECRET_REF_PREFIX}bbbb"),
            "/secret_key".to_string(),
            "Session token".to_string(),
            "Help from the API".to_string(),
        ] {
            assert!(redacted.contains(&expected), "{expected} in {redacted}");
        }
    }
}
