mod abort_transaction;
mod aggregate;
mod commit_transaction;
mod count;
mod count_documents;
mod create;
mod create_indexes;
mod delete;
mod distinct;
mod drop_collection;
mod drop_database;
mod drop_indexes;
mod find;
mod find_and_modify;
mod get_more;
mod insert;
mod list_collections;
mod list_databases;
mod list_indexes;
mod run_command;
mod update;

#[cfg(test)]
mod test;

use std::{collections::VecDeque, fmt::Debug, ops::Deref};

use bson::{RawBsonRef, RawDocument, RawDocumentBuf, Timestamp};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{
    bson::{self, Bson, Document},
    bson_util,
    client::{ClusterTime, HELLO_COMMAND_NAMES, REDACTED_COMMANDS},
    cmap::{conn::PinnedConnectionHandle, Command, RawCommandResponse, StreamDescription},
    error::{
        BulkWriteError,
        BulkWriteFailure,
        CommandError,
        Error,
        ErrorKind,
        Result,
        WriteConcernError,
        WriteFailure,
    },
    options::WriteConcern,
    selection_criteria::SelectionCriteria,
    Namespace,
};

pub(crate) use abort_transaction::AbortTransaction;
pub(crate) use aggregate::{Aggregate, AggregateTarget};
pub(crate) use commit_transaction::CommitTransaction;
pub(crate) use count::Count;
pub(crate) use count_documents::CountDocuments;
pub(crate) use create::Create;
pub(crate) use create_indexes::CreateIndexes;
pub(crate) use delete::Delete;
pub(crate) use distinct::Distinct;
pub(crate) use drop_collection::DropCollection;
pub(crate) use drop_database::DropDatabase;
pub(crate) use drop_indexes::DropIndexes;
pub(crate) use find::Find;
pub(crate) use find_and_modify::FindAndModify;
pub(crate) use get_more::GetMore;
pub(crate) use insert::Insert;
pub(crate) use list_collections::ListCollections;
pub(crate) use list_databases::ListDatabases;
pub(crate) use list_indexes::ListIndexes;
pub(crate) use run_command::RunCommand;
pub(crate) use update::Update;

const SERVER_4_9_0_WIRE_VERSION: i32 = 12;
const SERVER_4_2_0_WIRE_VERSION: i32 = 8;

/// A trait modeling the behavior of a server side operation.
pub(crate) trait Operation {
    /// The output type of this operation.
    type O;

    /// The format of the command body constructed in `build`.
    type Command: CommandBody;

    /// The name of the server side command associated with this operation.
    const NAME: &'static str;

    /// Returns the command that should be sent to the server as part of this operation.
    /// The operation may store some additional state that is required for handling the response.
    fn build(&mut self, description: &StreamDescription) -> Result<Command<Self::Command>>;

    /// Perform custom serialization of the built command.
    /// By default, this will just call through to the `Serialize` implementation of the command.
    fn serialize_command(&mut self, cmd: Command<Self::Command>) -> Result<Vec<u8>> {
        Ok(bson::to_vec(&cmd)?)
    }

    /// Parse the response for the atClusterTime field.
    /// Depending on the operation, this may be found in different locations.
    fn extract_at_cluster_time(&self, _response: &RawDocument) -> Result<Option<Timestamp>> {
        Ok(None)
    }

    /// Interprets the server response to the command.
    fn handle_response(
        &self,
        response: RawCommandResponse,
        description: &StreamDescription,
    ) -> Result<Self::O>;

    /// Interpret an error encountered while sending the built command to the server, potentially
    /// recovering.
    fn handle_error(&self, error: Error) -> Result<Self::O> {
        Err(error)
    }

    /// Criteria to use for selecting the server that this operation will be executed on.
    fn selection_criteria(&self) -> Option<&SelectionCriteria> {
        None
    }

    /// Whether or not this operation will request acknowledgment from the server.
    fn is_acknowledged(&self) -> bool {
        self.write_concern()
            .map(WriteConcern::is_acknowledged)
            .unwrap_or(true)
    }

    /// The write concern to use for this operation, if any.
    fn write_concern(&self) -> Option<&WriteConcern> {
        None
    }

    /// Returns whether or not this command supports the `readConcern` field.
    fn supports_read_concern(&self, _description: &StreamDescription) -> bool {
        false
    }

    /// Whether this operation supports sessions or not.
    fn supports_sessions(&self) -> bool {
        true
    }

    /// The level of retryability the operation supports.
    fn retryability(&self) -> Retryability {
        Retryability::None
    }

    /// Updates this operation as needed for a retry.
    fn update_for_retry(&mut self) {}

    fn pinned_connection(&self) -> Option<&PinnedConnectionHandle> {
        None
    }

    fn name(&self) -> &str {
        Self::NAME
    }
}

pub(crate) trait CommandBody: Serialize {
    fn should_redact(&self) -> bool {
        false
    }
}

impl CommandBody for Document {
    fn should_redact(&self) -> bool {
        if let Some(command_name) = bson_util::first_key(self) {
            HELLO_COMMAND_NAMES.contains(command_name.to_lowercase().as_str())
                && self.contains_key("speculativeAuthenticate")
        } else {
            false
        }
    }
}

impl<T: CommandBody> Command<T> {
    pub(crate) fn should_redact(&self) -> bool {
        let name = self.name.to_lowercase();
        REDACTED_COMMANDS.contains(name.as_str()) || self.body.should_redact()
    }

    pub(crate) fn should_compress(&self) -> bool {
        let name = self.name.to_lowercase();
        !REDACTED_COMMANDS.contains(name.as_str()) && !HELLO_COMMAND_NAMES.contains(name.as_str())
    }
}

/// A response to a command with a body shaped deserialized to a `T`.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommandResponse<T> {
    pub(crate) ok: Bson,

    #[serde(rename = "$clusterTime")]
    pub(crate) cluster_time: Option<ClusterTime>,

    #[serde(flatten)]
    pub(crate) body: T,
}

impl<T: DeserializeOwned> CommandResponse<T> {
    /// Whether the command succeeeded or not (i.e. if this response is ok: 1).
    pub(crate) fn is_success(&self) -> bool {
        bson_util::get_int(&self.ok) == Some(1)
    }

    pub(crate) fn cluster_time(&self) -> Option<&ClusterTime> {
        self.cluster_time.as_ref()
    }
}

/// A response body useful for deserializing command errors.
#[derive(Deserialize, Debug)]
pub(crate) struct CommandErrorBody {
    #[serde(rename = "errorLabels")]
    pub(crate) error_labels: Option<Vec<String>>,

    #[serde(flatten)]
    pub(crate) command_error: CommandError,
}

impl From<CommandErrorBody> for Error {
    fn from(command_error_response: CommandErrorBody) -> Error {
        Error::new(
            ErrorKind::Command(command_error_response.command_error),
            command_error_response.error_labels,
        )
    }
}

/// Appends a serializable struct to the input document.
/// The serializable struct MUST serialize to a Document, otherwise an error will be thrown.
pub(crate) fn append_options<T: Serialize + Debug>(
    doc: &mut Document,
    options: Option<&T>,
) -> Result<()> {
    match options {
        Some(options) => {
            let temp_doc = bson::to_bson(options)?;
            match temp_doc {
                Bson::Document(d) => {
                    doc.extend(d);
                    Ok(())
                }
                _ => Err(ErrorKind::Internal {
                    message: format!("options did not serialize to a Document: {:?}", options),
                }
                .into()),
            }
        }
        None => Ok(()),
    }
}

#[derive(Deserialize, Debug)]
pub(crate) struct EmptyBody {}

/// Body of a write response that could possibly have a write concern error but not write errors.
#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct WriteConcernOnlyBody {
    #[serde(rename = "writeConcernError")]
    write_concern_error: Option<WriteConcernError>,

    #[serde(rename = "errorLabels")]
    labels: Option<Vec<String>>,
}

impl WriteConcernOnlyBody {
    fn validate(&self) -> Result<()> {
        match self.write_concern_error {
            Some(ref wc_error) => Err(Error::new(
                ErrorKind::Write(WriteFailure::WriteConcernError(wc_error.clone())),
                self.labels.clone(),
            )),
            None => Ok(()),
        }
    }
}

#[derive(Deserialize, Debug)]
pub(crate) struct WriteResponseBody<T = EmptyBody> {
    #[serde(flatten)]
    body: T,

    n: u64,

    #[serde(rename = "writeErrors")]
    write_errors: Option<Vec<BulkWriteError>>,

    #[serde(rename = "writeConcernError")]
    write_concern_error: Option<WriteConcernError>,

    #[serde(rename = "errorLabels")]
    labels: Option<Vec<String>>,
}

impl<T> WriteResponseBody<T> {
    fn validate(&self) -> Result<()> {
        if self.write_errors.is_none() && self.write_concern_error.is_none() {
            return Ok(());
        };

        let failure = BulkWriteFailure {
            write_errors: self.write_errors.clone(),
            write_concern_error: self.write_concern_error.clone(),
            inserted_ids: Default::default(),
        };

        Err(Error::new(
            ErrorKind::BulkWrite(failure),
            self.labels.clone(),
        ))
    }
}

impl<T> Deref for WriteResponseBody<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.body
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CursorBody<T = RawDocumentBuf> {
    cursor: CursorInfo<T>,

    #[serde(flatten)]
    write_concern_info: WriteConcernOnlyBody,
}

impl CursorBody {
    fn extract_at_cluster_time(response: &RawDocument) -> Result<Option<Timestamp>> {
        Ok(response
            .get("cursor")?
            .and_then(RawBsonRef::as_document)
            .map(|d| d.get("atClusterTime"))
            .transpose()?
            .flatten()
            .and_then(RawBsonRef::as_timestamp))
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CursorInfo<T = RawDocumentBuf> {
    pub(crate) id: i64,

    pub(crate) ns: Namespace,

    pub(crate) first_batch: VecDeque<T>,

    pub(crate) post_batch_resume_token: Option<RawDocumentBuf>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Retryability {
    Write,
    Read,
    None,
}

macro_rules! remove_empty_write_concern {
    ($opts:expr) => {
        if let Some(ref mut options) = $opts {
            if let Some(ref write_concern) = options.write_concern {
                if write_concern.is_empty() {
                    options.write_concern = None;
                }
            }
        }
    };
}

pub(crate) use remove_empty_write_concern;
