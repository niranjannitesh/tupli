//! Changing things, when the connection allows it.
//!
//! Every function here is a thin wrapper over one command, and that is the
//! point: they all go through [`RedisConnection::command`], which refuses a
//! write on a read-only connection before anything reaches the socket. Redis
//! has no server-side read-only session to fall back on, so a code path that
//! built its own command and sent it another way would be a hole in the only
//! guardrail there is.
//!
//! The signatures take bytes rather than strings because keys, fields and
//! values are all binary, and a value that survived being read has to survive
//! being written back unchanged.

use db::DbResult;

use crate::client::{argv, RedisConnection};
use crate::resp::RespValue;

/// Set a string value, optionally with an expiry in seconds.
///
/// `KEEPTTL` when no expiry is given, so editing a session's contents does not
/// silently make it permanent — the surprise nobody wants from a value editor.
pub async fn set(
    conn: &RedisConnection,
    key: &[u8],
    value: &[u8],
    ttl: Option<u64>,
) -> DbResult<()> {
    let mut args = argv([b"SET", key, value]);
    match ttl {
        Some(seconds) => {
            args.push(b"EX".to_vec());
            args.push(seconds.to_string().into_bytes());
        }
        None => args.push(b"KEEPTTL".to_vec()),
    }
    conn.command(&args).await.map(|_| ())
}

/// Set one field of a hash. Returns whether the field was new.
pub async fn set_field(
    conn: &RedisConnection,
    key: &[u8],
    field: &[u8],
    value: &[u8],
) -> DbResult<bool> {
    let reply = conn.command(&argv([b"HSET", key, field, value])).await?;
    Ok(reply.as_i64().unwrap_or(0) > 0)
}

/// Replace one element of a list by index.
pub async fn set_index(
    conn: &RedisConnection,
    key: &[u8],
    index: i64,
    value: &[u8],
) -> DbResult<()> {
    conn.command(&argv([b"LSET", key, index.to_string().as_bytes(), value]))
        .await
        .map(|_| ())
}

/// Append to a list. `front` pushes to the head instead of the tail.
pub async fn push(
    conn: &RedisConnection,
    key: &[u8],
    value: &[u8],
    front: bool,
) -> DbResult<u64> {
    let command: &[u8] = if front { b"LPUSH" } else { b"RPUSH" };
    let reply = conn.command(&argv([command, key, value])).await?;
    Ok(reply.as_i64().unwrap_or(0).max(0) as u64)
}

/// Add a member to a set. Returns whether it was not already there.
pub async fn add_member(conn: &RedisConnection, key: &[u8], member: &[u8]) -> DbResult<bool> {
    let reply = conn.command(&argv([b"SADD", key, member])).await?;
    Ok(reply.as_i64().unwrap_or(0) > 0)
}

/// Set a member's score in a sorted set, adding it if it is new.
pub async fn set_score(
    conn: &RedisConnection,
    key: &[u8],
    member: &[u8],
    score: f64,
) -> DbResult<()> {
    let score = db::value::format_f64(score);
    conn.command(&argv([b"ZADD", key, score.as_bytes(), member]))
        .await
        .map(|_| ())
}

/// Append an entry to a stream, returning the id the server gave it.
///
/// `id` is `*` unless the caller has a reason — a stream's ids have to
/// increase, so a chosen one is usually a mistake and always a decision.
pub async fn append_entry(
    conn: &RedisConnection,
    key: &[u8],
    id: Option<&str>,
    fields: &[(Vec<u8>, Vec<u8>)],
) -> DbResult<String> {
    let mut args = argv([b"XADD", key, id.unwrap_or("*").as_bytes()]);
    for (field, value) in fields {
        args.push(field.clone());
        args.push(value.clone());
    }
    let reply = conn.command(&args).await?;
    Ok(reply.as_str().unwrap_or_default().to_string())
}

/// Replace a member of a set, as one step.
///
/// Two commands, but inside `MULTI`/`EXEC`: a set has no update, and a pane
/// that briefly showed neither the old member nor the new one would be showing
/// a state that never existed.
pub async fn replace_member(
    conn: &RedisConnection,
    key: &[u8],
    old: &[u8],
    new: &[u8],
) -> DbResult<()> {
    conn.transaction(&[argv([b"SREM", key, old]), argv([b"SADD", key, new])])
        .await
        .map(|_| ())
}

/// Rename a set member while keeping its score, as one step.
pub async fn rename_member(
    conn: &RedisConnection,
    key: &[u8],
    old: &[u8],
    new: &[u8],
    score: f64,
) -> DbResult<()> {
    let score = db::value::format_f64(score);
    conn.transaction(&[
        argv([b"ZREM", key, old]),
        argv([b"ZADD", key, score.as_bytes(), new]),
    ])
    .await
    .map(|_| ())
}

/// Delete keys, returning how many existed.
///
/// `UNLINK` rather than `DEL`: the memory is reclaimed on another thread, so
/// deleting a key with ten million elements does not stop the server while it
/// happens. Older servers do not have it and get `DEL` instead.
pub async fn delete(conn: &RedisConnection, keys: &[Vec<u8>]) -> DbResult<u64> {
    if keys.is_empty() {
        return Ok(0);
    }
    let command = |name: &[u8]| {
        let mut args = vec![name.to_vec()];
        args.extend(keys.iter().cloned());
        args
    };
    let reply = match conn.command(&command(b"UNLINK")).await {
        Ok(reply) => reply,
        Err(error) if error.class == db::ErrorClass::Syntax => {
            conn.command(&command(b"DEL")).await?
        }
        Err(error) => return Err(error),
    };
    Ok(reply.as_i64().unwrap_or(0).max(0) as u64)
}

/// Remove one field from a hash.
pub async fn remove_field(conn: &RedisConnection, key: &[u8], field: &[u8]) -> DbResult<bool> {
    let reply = conn.command(&argv([b"HDEL", key, field])).await?;
    Ok(reply.as_i64().unwrap_or(0) > 0)
}

/// Remove one member from a set or a sorted set.
pub async fn remove_member(
    conn: &RedisConnection,
    key: &[u8],
    member: &[u8],
    sorted: bool,
) -> DbResult<bool> {
    let command: &[u8] = if sorted { b"ZREM" } else { b"SREM" };
    let reply = conn.command(&argv([command, key, member])).await?;
    Ok(reply.as_i64().unwrap_or(0) > 0)
}

/// Remove one entry from a stream.
pub async fn remove_entry(conn: &RedisConnection, key: &[u8], id: &str) -> DbResult<bool> {
    let reply = conn.command(&argv([b"XDEL", key, id.as_bytes()])).await?;
    Ok(reply.as_i64().unwrap_or(0) > 0)
}

/// Remove one element from a list.
///
/// Lists have no delete-by-index. The idiom is to overwrite the element with a
/// sentinel nothing else can be and then remove that, which is why this is a
/// transaction and not a command.
pub async fn remove_index(conn: &RedisConnection, key: &[u8], index: i64) -> DbResult<()> {
    let sentinel = b"__tupli_deleted__".to_vec();
    conn.transaction(&[
        argv([b"LSET", key, index.to_string().as_bytes(), &sentinel]),
        argv([b"LREM", key, b"1", &sentinel]),
    ])
    .await
    .map(|_| ())
}

/// Give a key an expiry, in seconds from now.
pub async fn expire(conn: &RedisConnection, key: &[u8], seconds: u64) -> DbResult<bool> {
    let reply = conn
        .command(&argv([b"EXPIRE", key, seconds.to_string().as_bytes()]))
        .await?;
    Ok(reply.as_i64().unwrap_or(0) > 0)
}

/// Take a key's expiry away, so it stops being temporary.
pub async fn persist(conn: &RedisConnection, key: &[u8]) -> DbResult<bool> {
    let reply = conn.command(&argv([b"PERSIST", key])).await?;
    Ok(reply.as_i64().unwrap_or(0) > 0)
}

/// Rename a key.
///
/// `overwrite` picks between `RENAME`, which replaces whatever is at the new
/// name, and `RENAMENX`, which refuses. The default in the UI should be to
/// refuse: renaming a key onto another one deletes the second, silently, and
/// no undo exists.
pub async fn rename(
    conn: &RedisConnection,
    key: &[u8],
    to: &[u8],
    overwrite: bool,
) -> DbResult<bool> {
    let command: &[u8] = if overwrite { b"RENAME" } else { b"RENAMENX" };
    let reply = conn.command(&argv([command, key, to])).await?;
    Ok(match reply {
        // `RENAME` says OK; `RENAMENX` says whether it did anything.
        RespValue::Int(done) => done > 0,
        _ => true,
    })
}
