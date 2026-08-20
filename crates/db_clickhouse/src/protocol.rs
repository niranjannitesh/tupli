//! Packet tags and the revision this client speaks.
//!
//! ClickHouse versions its wire protocol with a single monotonic number, and
//! *both* sides gate their field lists on the number the **client** sent. That
//! is the fact this module is built around: the revision below is not a claim
//! about what the server can do, it is a request for a particular shape of
//! conversation, and the server honours it going back years.
//!
//! So the revision is chosen to be the *lowest* one that still gives a browser
//! everything it needs, rather than the highest one that exists. Every step
//! above [`CLIENT_REVISION`] adds protocol surface — parallel-replica routing,
//! sparse column serialisation, chunked packets — that a client which reads
//! tables and runs `select` gains nothing from and can only get wrong.

/// What this client tells the server it speaks.
///
/// 54440 is one below [`MIN_REVISION_WITH_INTERSERVER_SECRET`], which puts it
/// under every revision that would cost real work and above every revision
/// that gives a browser anything:
///
/// - 54441 makes the server send a nonce in its `Hello` and expects a secret
///   in every `Query` — machinery for one node authenticating to another,
///   which this client is not.
/// - 54453 adds three parallel-replica fields to every `ClientInfo`.
/// - 54454 puts a custom-serialisation flag in front of every column in every
///   block, and a `1` there means a sparse encoding this reads nothing of.
/// - 54458 adds an addendum packet after the handshake.
///
/// What it keeps is the two things that matter: 54401, so the server's patch
/// version is known, and 54420, so `Progress` reports rows *written* — the
/// only honest source for what an `insert` did.
///
/// Asking for less is not a compatibility hack; it is the protocol working as
/// designed. Raising this later means adding the gated fields below, which is
/// why every one of them is written as a gate rather than assumed.
pub const CLIENT_REVISION: u64 = 54440;

/// What this client calls itself in `system.query_log`. Worth getting right:
/// it is how somebody staring at a slow query works out which window sent it.
pub const CLIENT_NAME: &str = "tupli";
pub const CLIENT_VERSION_MAJOR: u64 = 0;
pub const CLIENT_VERSION_MINOR: u64 = 1;
pub const CLIENT_VERSION_PATCH: u64 = 0;

/// The server started sending its time zone in the handshake.
pub const MIN_REVISION_WITH_SERVER_TIMEZONE: u64 = 54058;
/// `ClientInfo` gained a quota key.
pub const MIN_REVISION_WITH_QUOTA_KEY_IN_CLIENT_INFO: u64 = 54060;
/// A block carries a `BlockInfo` header before its columns.
pub const MIN_REVISION_WITH_BLOCK_INFO: u64 = 51903;
/// A `Data` packet is prefixed by the name of the temporary table it belongs
/// to — empty for the ordinary case, which is every case here.
pub const MIN_REVISION_WITH_TEMPORARY_TABLES: u64 = 50264;
/// The `Query` packet carries a `ClientInfo`.
pub const MIN_REVISION_WITH_CLIENT_INFO: u64 = 54032;
/// The server sends a display name in the handshake.
pub const MIN_REVISION_WITH_SERVER_DISPLAY_NAME: u64 = 54372;
/// Both sides send a patch version.
pub const MIN_REVISION_WITH_VERSION_PATCH: u64 = 54401;
/// `Progress` reports rows written as well as rows read — the one honest
/// source for "how many rows did that `insert` actually write".
pub const MIN_REVISION_WITH_CLIENT_WRITE_INFO: u64 = 54420;
/// The server sends a nonce in its `Hello` and the `Query` packet carries an
/// inter-server secret. The first revision this client deliberately does not
/// reach — see [`CLIENT_REVISION`].
pub const MIN_REVISION_WITH_INTERSERVER_SECRET: u64 = 54441;
/// `ClientInfo` carries an OpenTelemetry trace flag.
pub const MIN_REVISION_WITH_OPENTELEMETRY: u64 = 54442;
/// `ClientInfo` carries how many hops a distributed query has taken.
pub const MIN_REVISION_WITH_DISTRIBUTED_DEPTH: u64 = 54448;
/// `ClientInfo` carries when the initial query started.
pub const MIN_REVISION_WITH_INITIAL_QUERY_START_TIME: u64 = 54449;
/// `ClientInfo` carries parallel-replica routing.
pub const MIN_REVISION_WITH_PARALLEL_REPLICAS: u64 = 54453;

/// What the client can send.
pub mod client {
    pub const HELLO: u64 = 0;
    pub const QUERY: u64 = 1;
    pub const DATA: u64 = 2;
    pub const CANCEL: u64 = 3;
    pub const PING: u64 = 4;
}

/// What the server can send.
pub mod server {
    pub const HELLO: u64 = 0;
    pub const DATA: u64 = 1;
    pub const EXCEPTION: u64 = 2;
    pub const PROGRESS: u64 = 3;
    pub const PONG: u64 = 4;
    pub const END_OF_STREAM: u64 = 5;
    pub const PROFILE_INFO: u64 = 6;
    pub const TOTALS: u64 = 7;
    pub const EXTREMES: u64 = 8;
    pub const TABLES_STATUS_RESPONSE: u64 = 9;
    pub const LOG: u64 = 10;
    pub const TABLE_COLUMNS: u64 = 11;
    pub const PART_UUIDS: u64 = 12;
    pub const READ_TASK_REQUEST: u64 = 13;
    pub const PROFILE_EVENTS: u64 = 14;

    /// What a tag means, for an error message. A packet this client does not
    /// know how to skip has to end the read: the protocol has no lengths, so
    /// "ignore it" is not available — the next byte would be read as something
    /// it is not.
    pub fn name(tag: u64) -> &'static str {
        match tag {
            HELLO => "Hello",
            DATA => "Data",
            EXCEPTION => "Exception",
            PROGRESS => "Progress",
            PONG => "Pong",
            END_OF_STREAM => "EndOfStream",
            PROFILE_INFO => "ProfileInfo",
            TOTALS => "Totals",
            EXTREMES => "Extremes",
            TABLES_STATUS_RESPONSE => "TablesStatusResponse",
            LOG => "Log",
            TABLE_COLUMNS => "TableColumns",
            PART_UUIDS => "PartUUIDs",
            READ_TASK_REQUEST => "ReadTaskRequest",
            PROFILE_EVENTS => "ProfileEvents",
            _ => "an unknown packet",
        }
    }
}

/// How much of the query the server should run before answering. Anything
/// other than `Complete` is for the internals of a distributed query.
pub const STAGE_COMPLETE: u64 = 2;

/// The `query_kind` of a query somebody typed, as opposed to one a node sent
/// another node.
pub const QUERY_KIND_INITIAL: u8 = 1;

/// The `interface` a query arrived over. The other value is HTTP, which is
/// the interface this driver exists not to use.
pub const INTERFACE_TCP: u8 = 1;
