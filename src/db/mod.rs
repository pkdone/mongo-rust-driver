pub mod options;

use std::{fmt::Debug, sync::Arc};

use futures_util::stream::TryStreamExt;

use crate::{
    bson::{Bson, Document},
    change_stream::{
        event::ChangeStreamEvent,
        options::ChangeStreamOptions,
        session::SessionChangeStream,
        ChangeStream,
    },
    client::session::TransactionState,
    cmap::conn::PinnedConnectionHandle,
    concern::{ReadConcern, WriteConcern},
    cursor::Cursor,
    error::{Error, ErrorKind, Result},
    operation::{Aggregate, AggregateTarget, Create, DropDatabase, ListCollections, RunCommand},
    options::{
        AggregateOptions,
        CollectionOptions,
        CreateCollectionOptions,
        DatabaseOptions,
        DropDatabaseOptions,
        ListCollectionsOptions,
    },
    results::CollectionSpecification,
    selection_criteria::SelectionCriteria,
    Client,
    ClientSession,
    Collection,
    Namespace,
    SessionCursor,
};

/// `Database` is the client-side abstraction of a MongoDB database. It can be used to perform
/// database-level operations or to obtain handles to specific collections within the database. A
/// `Database` can only be obtained through a [`Client`](struct.Client.html) by calling either
/// [`Client::database`](struct.Client.html#method.database) or
/// [`Client::database_with_options`](struct.Client.html#method.database_with_options).
///
/// `Database` uses [`std::sync::Arc`](https://doc.rust-lang.org/std/sync/struct.Arc.html) internally,
/// so it can safely be shared across threads or async tasks. For example:
///
/// ```rust
/// 
/// # #[cfg(not(feature = "sync"))]
/// # use mongodb::{bson::Document, Client, error::Result};
/// # #[cfg(feature = "async-std-runtime")]
/// # use async_std::task;
/// # #[cfg(feature = "tokio-runtime")]
/// # use tokio::task;
/// #
/// #
/// # #[cfg(not(feature = "sync"))]
/// # async fn start_workers() -> Result<()> {
/// # let client = Client::with_uri_str("mongodb://example.com").await?;
/// let db = client.database("items");
///
/// for i in 0..5 {
///     let db_ref = db.clone();
///
///     task::spawn(async move {
///         let collection = db_ref.collection::<Document>(&format!("coll{}", i));
///
///         // Do something with the collection
///     });
/// }
/// #
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Database {
    inner: Arc<DatabaseInner>,
}

#[derive(Debug)]
struct DatabaseInner {
    client: Client,
    name: String,
    selection_criteria: Option<SelectionCriteria>,
    read_concern: Option<ReadConcern>,
    write_concern: Option<WriteConcern>,
}

impl Database {
    pub(crate) fn new(client: Client, name: &str, options: Option<DatabaseOptions>) -> Self {
        let options = options.unwrap_or_default();
        let selection_criteria = options
            .selection_criteria
            .or_else(|| client.selection_criteria().cloned());

        let read_concern = options
            .read_concern
            .or_else(|| client.read_concern().cloned());

        let write_concern = options
            .write_concern
            .or_else(|| client.write_concern().cloned());

        Self {
            inner: Arc::new(DatabaseInner {
                client,
                name: name.to_string(),
                selection_criteria,
                read_concern,
                write_concern,
            }),
        }
    }

    /// Get the `Client` that this collection descended from.
    pub(crate) fn client(&self) -> &Client {
        &self.inner.client
    }

    /// Gets the name of the `Database`.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Gets the read preference of the `Database`.
    pub fn selection_criteria(&self) -> Option<&SelectionCriteria> {
        self.inner.selection_criteria.as_ref()
    }

    /// Gets the read concern of the `Database`.
    pub fn read_concern(&self) -> Option<&ReadConcern> {
        self.inner.read_concern.as_ref()
    }

    /// Gets the write concern of the `Database`.
    pub fn write_concern(&self) -> Option<&WriteConcern> {
        self.inner.write_concern.as_ref()
    }

    /// Gets a handle to a collection in this database with the provided name. The
    /// [`Collection`] options (e.g. read preference and write concern) will default to those of
    /// this [`Database`].
    ///
    /// For more information on how the generic parameter `T` is used, check out the [`Collection`]
    /// documentation.
    ///
    /// This method does not send or receive anything across the wire to the database, so it can be
    /// used repeatedly without incurring any costs from I/O.
    pub fn collection<T>(&self, name: &str) -> Collection<T> {
        Collection::new(self.clone(), name, None)
    }

    /// Gets a handle to a collection in this database with the provided name.
    /// Operations done with this `Collection` will use the options specified by
    /// `options` and will otherwise default to those of this [`Database`].
    ///
    /// For more information on how the generic parameter `T` is used, check out the [`Collection`]
    /// documentation.
    ///
    /// This method does not send or receive anything across the wire to the database, so it can be
    /// used repeatedly without incurring any costs from I/O.
    pub fn collection_with_options<T>(
        &self,
        name: &str,
        options: CollectionOptions,
    ) -> Collection<T> {
        Collection::new(self.clone(), name, Some(options))
    }

    async fn drop_common(
        &self,
        options: impl Into<Option<DropDatabaseOptions>>,
        session: impl Into<Option<&mut ClientSession>>,
    ) -> Result<()> {
        let mut options = options.into();
        resolve_options!(self, options, [write_concern]);

        let drop_database = DropDatabase::new(self.name().to_string(), options);
        self.client()
            .execute_operation(drop_database, session)
            .await
    }

    /// Drops the database, deleting all data, collections, and indexes stored in it.
    pub async fn drop(&self, options: impl Into<Option<DropDatabaseOptions>>) -> Result<()> {
        self.drop_common(options, None).await
    }

    /// Drops the database, deleting all data, collections, and indexes stored in it using the
    /// provided `ClientSession`.
    pub async fn drop_with_session(
        &self,
        options: impl Into<Option<DropDatabaseOptions>>,
        session: &mut ClientSession,
    ) -> Result<()> {
        self.drop_common(options, session).await
    }

    /// Gets information about each of the collections in the database. The cursor will yield a
    /// document pertaining to each collection in the database.
    pub async fn list_collections(
        &self,
        filter: impl Into<Option<Document>>,
        options: impl Into<Option<ListCollectionsOptions>>,
    ) -> Result<Cursor<CollectionSpecification>> {
        let list_collections = ListCollections::new(
            self.name().to_string(),
            filter.into(),
            false,
            options.into(),
        );
        self.client()
            .execute_cursor_operation(list_collections)
            .await
    }

    /// Gets information about each of the collections in the database using the provided
    /// `ClientSession`. The cursor will yield a document pertaining to each collection in the
    /// database.
    pub async fn list_collections_with_session(
        &self,
        filter: impl Into<Option<Document>>,
        options: impl Into<Option<ListCollectionsOptions>>,
        session: &mut ClientSession,
    ) -> Result<SessionCursor<CollectionSpecification>> {
        let list_collections = ListCollections::new(
            self.name().to_string(),
            filter.into(),
            false,
            options.into(),
        );
        self.client()
            .execute_session_cursor_operation(list_collections, session)
            .await
    }

    async fn list_collection_names_common(
        &self,
        cursor: impl TryStreamExt<Ok = Document, Error = Error>,
    ) -> Result<Vec<String>> {
        cursor
            .and_then(|doc| match doc.get("name").and_then(Bson::as_str) {
                Some(name) => futures_util::future::ok(name.into()),
                None => futures_util::future::err(
                    ErrorKind::InvalidResponse {
                        message: "Expected name field in server response, but there was none."
                            .to_string(),
                    }
                    .into(),
                ),
            })
            .try_collect()
            .await
    }

    /// Gets the names of the collections in the database.
    pub async fn list_collection_names(
        &self,
        filter: impl Into<Option<Document>>,
    ) -> Result<Vec<String>> {
        let list_collections =
            ListCollections::new(self.name().to_string(), filter.into(), true, None);
        let cursor: Cursor<Document> = self
            .client()
            .execute_cursor_operation(list_collections)
            .await?;

        self.list_collection_names_common(cursor).await
    }

    /// Gets the names of the collections in the database using the provided `ClientSession`.
    pub async fn list_collection_names_with_session(
        &self,
        filter: impl Into<Option<Document>>,
        session: &mut ClientSession,
    ) -> Result<Vec<String>> {
        let list_collections =
            ListCollections::new(self.name().to_string(), filter.into(), true, None);
        let mut cursor: SessionCursor<Document> = self
            .client()
            .execute_session_cursor_operation(list_collections, &mut *session)
            .await?;

        self.list_collection_names_common(cursor.stream(session))
            .await
    }

    async fn create_collection_common(
        &self,
        name: impl AsRef<str>,
        options: impl Into<Option<CreateCollectionOptions>>,
        session: impl Into<Option<&mut ClientSession>>,
    ) -> Result<()> {
        let mut options = options.into();
        resolve_options!(self, options, [write_concern]);

        let create = Create::new(
            Namespace {
                db: self.name().to_string(),
                coll: name.as_ref().to_string(),
            },
            options,
        );
        self.client().execute_operation(create, session).await
    }

    /// Creates a new collection in the database with the given `name` and `options`.
    ///
    /// Note that MongoDB creates collections implicitly when data is inserted, so this method is
    /// not needed if no special options are required.
    pub async fn create_collection(
        &self,
        name: impl AsRef<str>,
        options: impl Into<Option<CreateCollectionOptions>>,
    ) -> Result<()> {
        self.create_collection_common(name, options, None).await
    }

    /// Creates a new collection in the database with the given `name` and `options` using the
    /// provided `ClientSession`.
    ///
    /// Note that MongoDB creates collections implicitly when data is inserted, so this method is
    /// not needed if no special options are required.
    pub async fn create_collection_with_session(
        &self,
        name: impl AsRef<str>,
        options: impl Into<Option<CreateCollectionOptions>>,
        session: &mut ClientSession,
    ) -> Result<()> {
        self.create_collection_common(name, options, session).await
    }

    pub(crate) async fn run_command_common(
        &self,
        command: Document,
        selection_criteria: impl Into<Option<SelectionCriteria>>,
        session: impl Into<Option<&mut ClientSession>>,
        pinned_connection: Option<&PinnedConnectionHandle>,
    ) -> Result<Document> {
        let operation = RunCommand::new(
            self.name().into(),
            command,
            selection_criteria.into(),
            pinned_connection,
        )?;
        self.client().execute_operation(operation, session).await
    }

    /// Runs a database-level command.
    ///
    /// Note that no inspection is done on `doc`, so the command will not use the database's default
    /// read concern or write concern. If specific read concern or write concern is desired, it must
    /// be specified manually.
    pub async fn run_command(
        &self,
        command: Document,
        selection_criteria: impl Into<Option<SelectionCriteria>>,
    ) -> Result<Document> {
        self.run_command_common(command, selection_criteria, None, None)
            .await
    }

    /// Runs a database-level command using the provided `ClientSession`.
    ///
    /// If the `ClientSession` provided is currently in a transaction, `command` must not specify a
    /// read concern. If this operation is the first operation in the transaction, the read concern
    /// associated with the transaction will be inherited.
    ///
    /// Otherwise no inspection is done on `command`, so the command will not use the database's
    /// default read concern or write concern. If specific read concern or write concern is
    /// desired, it must be specified manually.
    pub async fn run_command_with_session(
        &self,
        command: Document,
        selection_criteria: impl Into<Option<SelectionCriteria>>,
        session: &mut ClientSession,
    ) -> Result<Document> {
        let mut selection_criteria = selection_criteria.into();
        match session.transaction.state {
            TransactionState::Starting | TransactionState::InProgress => {
                if command.contains_key("readConcern") {
                    return Err(ErrorKind::InvalidArgument {
                        message: "Cannot set read concern after starting a transaction".into(),
                    }
                    .into());
                }
                selection_criteria = match selection_criteria {
                    Some(selection_criteria) => Some(selection_criteria),
                    None => {
                        if let Some(ref options) = session.transaction.options {
                            options.selection_criteria.clone()
                        } else {
                            None
                        }
                    }
                };
            }
            _ => {}
        }
        self.run_command_common(command, selection_criteria, session, None)
            .await
    }

    /// Runs an aggregation operation.
    ///
    /// See the documentation [here](https://docs.mongodb.com/manual/aggregation/) for more
    /// information on aggregations.
    pub async fn aggregate(
        &self,
        pipeline: impl IntoIterator<Item = Document>,
        options: impl Into<Option<AggregateOptions>>,
    ) -> Result<Cursor<Document>> {
        let mut options = options.into();
        resolve_options!(
            self,
            options,
            [read_concern, write_concern, selection_criteria]
        );

        let aggregate = Aggregate::new(self.name().to_string(), pipeline, options);
        let client = self.client();
        client.execute_cursor_operation(aggregate).await
    }

    /// Runs an aggregation operation with the provided `ClientSession`.
    ///
    /// See the documentation [here](https://docs.mongodb.com/manual/aggregation/) for more
    /// information on aggregations.
    pub async fn aggregate_with_session(
        &self,
        pipeline: impl IntoIterator<Item = Document>,
        options: impl Into<Option<AggregateOptions>>,
        session: &mut ClientSession,
    ) -> Result<SessionCursor<Document>> {
        let mut options = options.into();
        resolve_options!(
            self,
            options,
            [read_concern, write_concern, selection_criteria]
        );

        let aggregate = Aggregate::new(self.name().to_string(), pipeline, options);
        let client = self.client();
        client
            .execute_session_cursor_operation(aggregate, session)
            .await
    }

    /// Starts a new [`ChangeStream`](change_stream/struct.ChangeStream.html) that receives events
    /// for all changes in this database. The stream does not observe changes from system
    /// collections and cannot be started on "config", "local" or "admin" databases.
    ///
    /// See the documentation [here](https://docs.mongodb.com/manual/changeStreams/) on change
    /// streams.
    ///
    /// Change streams require either a "majority" read concern or no read
    /// concern. Anything else will cause a server error.
    ///
    /// Note that using a `$project` stage to remove any of the `_id`, `operationType` or `ns`
    /// fields will cause an error. The driver requires these fields to support resumability. For
    /// more information on resumability, see the documentation for
    /// [`ChangeStream`](change_stream/struct.ChangeStream.html)
    ///
    /// If the pipeline alters the structure of the returned events, the parsed type will need to be
    /// changed via [`ChangeStream::with_type`].
    #[allow(unused)]
    pub(crate) async fn watch(
        &self,
        pipeline: impl IntoIterator<Item = Document>,
        options: impl Into<Option<ChangeStreamOptions>>,
    ) -> Result<ChangeStream<ChangeStreamEvent<Document>>> {
        let mut options = options.into();
        resolve_options!(self, options, [read_concern, selection_criteria]);
        let target = AggregateTarget::Database(self.name().to_string());
        self.client().execute_watch(pipeline, options, target).await
    }

    /// Starts a new [`SessionChangeStream`] that receives events for all changes in this database
    /// using the provided [`ClientSession`].  See [`Database::watch`] for more information.
    #[allow(unused)]
    pub(crate) async fn watch_with_session(
        &self,
        pipeline: impl IntoIterator<Item = Document>,
        options: impl Into<Option<ChangeStreamOptions>>,
        session: &mut ClientSession,
    ) -> Result<SessionChangeStream<ChangeStreamEvent<Document>>> {
        let mut options = options.into();
        resolve_read_concern_with_session!(self, options, Some(&mut *session))?;
        resolve_selection_criteria_with_session!(self, options, Some(&mut *session))?;
        let target = AggregateTarget::Database(self.name().to_string());
        self.client()
            .execute_watch_with_session(pipeline, options, target, session)
            .await
    }
}
