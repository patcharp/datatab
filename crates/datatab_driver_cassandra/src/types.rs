//! Mapping between CQL values and the driver-neutral value and column types.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use datatab_core::{ColumnKind, ColumnMeta, DbError, Value};
use scylla::frame::response::result::{CollectionType, ColumnSpec, ColumnType, NativeType};
use scylla::value::{CqlDate, CqlDuration, CqlTime, CqlTimeuuid, CqlValue};

/// The largest decimal scale this driver renders. A server scale past this is
/// treated as undecodable instead of asking for gigabytes of padding.
const MAX_DECIMAL_SCALE: u32 = 1_000;

/// Column metadata for one column of a result set.
///
/// CQL result metadata carries no nullability or key information: a projection
/// can drop the primary key and nothing marks a column `NOT NULL`. Both flags
/// therefore stay at their neutral values rather than guessing.
pub(crate) fn column_meta_from_spec(spec: &ColumnSpec<'_>) -> ColumnMeta {
    ColumnMeta {
        name: spec.name().to_string(),
        type_name: column_type_name(spec.typ()),
        kind: column_kind(spec.typ()),
        nullable: true,
        is_primary_key: false,
    }
}

/// The CQL type name as the server would write it, for example `list<int>`.
pub(crate) fn column_type_name(column_type: &ColumnType<'_>) -> String {
    match column_type {
        ColumnType::Native(native) => native_type_name(native).to_string(),
        ColumnType::Collection { frozen, typ } => {
            let inner = match typ {
                CollectionType::List(element) => format!("list<{}>", column_type_name(element)),
                CollectionType::Set(element) => format!("set<{}>", column_type_name(element)),
                CollectionType::Map(key, value) => {
                    format!(
                        "map<{}, {}>",
                        column_type_name(key),
                        column_type_name(value)
                    )
                }
                // `CollectionType` is `#[non_exhaustive]`.
                _ => "collection<unknown>".to_string(),
            };
            if *frozen {
                format!("frozen<{inner}>")
            } else {
                inner
            }
        }
        ColumnType::Vector { typ, dimensions } => {
            format!("vector<{}, {dimensions}>", column_type_name(typ))
        }
        ColumnType::Tuple(types) => {
            let inner = types
                .iter()
                .map(column_type_name)
                .collect::<Vec<_>>()
                .join(", ");
            format!("tuple<{inner}>")
        }
        ColumnType::UserDefinedType { definition, .. } => definition.name.to_string(),
        // The enum is `#[non_exhaustive]`: a type a newer protocol adds has no
        // name this driver knows.
        _ => "unknown".to_string(),
    }
}

fn native_type_name(native: &NativeType) -> &'static str {
    match native {
        NativeType::Ascii => "ascii",
        NativeType::Boolean => "boolean",
        NativeType::Blob => "blob",
        NativeType::Counter => "counter",
        NativeType::Date => "date",
        NativeType::Decimal => "decimal",
        NativeType::Double => "double",
        NativeType::Duration => "duration",
        NativeType::Float => "float",
        NativeType::Int => "int",
        NativeType::BigInt => "bigint",
        NativeType::Text => "text",
        NativeType::Timestamp => "timestamp",
        NativeType::Inet => "inet",
        NativeType::SmallInt => "smallint",
        NativeType::TinyInt => "tinyint",
        NativeType::Time => "time",
        NativeType::Timeuuid => "timeuuid",
        NativeType::Uuid => "uuid",
        NativeType::Varint => "varint",
        _ => "unknown",
    }
}

/// The chart engine's classification. Types it cannot plot stay `Unknown`
/// rather than being forced into a numeric or text bucket.
///
/// Every CQL native type has its arm here; a collection, a tuple, a vector, a
/// user type or a type a later protocol adds stays `Unknown`.
pub(crate) fn column_kind(column_type: &ColumnType<'_>) -> ColumnKind {
    let ColumnType::Native(native) = column_type else {
        return ColumnKind::Unknown;
    };
    match native {
        // Design §3: the chart engine plots numbers and timestamps; identifiers,
        // addresses, `time` and `duration` are text, and the rest stay unknown.
        NativeType::Ascii
        | NativeType::Text
        | NativeType::Inet
        | NativeType::Uuid
        | NativeType::Timeuuid
        | NativeType::Time
        | NativeType::Duration => ColumnKind::Text,
        NativeType::Int
        | NativeType::BigInt
        | NativeType::SmallInt
        | NativeType::TinyInt
        | NativeType::Counter
        | NativeType::Varint => ColumnKind::Integer,
        NativeType::Float | NativeType::Double | NativeType::Decimal => ColumnKind::Float,
        NativeType::Timestamp | NativeType::Date => ColumnKind::Timestamp,
        NativeType::Blob | NativeType::Boolean => ColumnKind::Unknown,
        _ => ColumnKind::Unknown,
    }
}

/// Converts one cell. `None` is SQL's NULL.
pub(crate) fn value_from_cql(value: &CqlValue) -> Value {
    match value {
        CqlValue::Ascii(text) | CqlValue::Text(text) => Value::Text(text.clone()),
        CqlValue::Boolean(flag) => Value::Bool(*flag),
        CqlValue::Blob(bytes) => Value::Bytes(bytes.clone()),
        CqlValue::Counter(counter) => Value::Int(counter.0),
        CqlValue::Decimal(decimal) => {
            let (bytes, scale) = decimal.as_signed_be_bytes_slice_and_exponent();
            match big_endian_to_i128(bytes) {
                Some(unscaled) => match format_decimal(unscaled, scale) {
                    Some(text) => Value::Decimal(text),
                    None => Value::Unsupported("decimal".to_string()),
                },
                // Wider than 16 bytes: no exact decimal string without a
                // big-integer dependency, so the cell is marked undecodable
                // rather than rounded.
                None => Value::Unsupported("decimal".to_string()),
            }
        }
        CqlValue::Date(date) => {
            // The crate converts into `chrono::NaiveDate` through `TryInto`;
            // the whole CQL date range does not fit, so a wider value is
            // reported as undecodable instead of being clamped.
            let parsed: Result<NaiveDate, _> = (*date).try_into();
            match parsed {
                Ok(date) => Value::Date(date),
                Err(_) => Value::Unsupported("date".to_string()),
            }
        }
        CqlValue::Double(value) => Value::Float(*value),
        CqlValue::Duration(duration) => Value::Text(format_duration(*duration)),
        // An empty element inside a collection carries no bytes, so it shows as
        // an empty text cell rather than a null.
        CqlValue::Empty => Value::Text(String::new()),
        CqlValue::Float(value) => Value::Float(f64::from(*value)),
        CqlValue::Int(value) => Value::Int(i64::from(*value)),
        CqlValue::BigInt(value) => Value::Int(*value),
        CqlValue::Timestamp(timestamp) => match timestamp_to_datetime(*timestamp) {
            Some(date_time) => Value::DateTime(date_time),
            None => Value::Unsupported("timestamp".to_string()),
        },
        CqlValue::Inet(address) => Value::Text(address.to_string()),
        CqlValue::List(items) | CqlValue::Set(items) | CqlValue::Vector(items) => {
            Value::Array(items.iter().map(value_from_cql).collect())
        }
        CqlValue::Map(entries) => {
            let mut map = BTreeMap::new();
            for (key, value) in entries {
                map.insert(
                    value_from_cql(key).as_display_string(),
                    value_from_cql(value),
                );
            }
            Value::Document(map)
        }
        CqlValue::UserDefinedType { fields, .. } => {
            let mut document = BTreeMap::new();
            for (name, value) in fields {
                document.insert(
                    name.clone(),
                    value.as_ref().map(value_from_cql).unwrap_or(Value::Null),
                );
            }
            Value::Document(document)
        }
        CqlValue::SmallInt(value) => Value::Int(i64::from(*value)),
        CqlValue::TinyInt(value) => Value::Int(i64::from(*value)),
        CqlValue::Time(time) => match time_to_naive(*time) {
            Some(time) => Value::Time(time),
            None => Value::Unsupported("time".to_string()),
        },
        CqlValue::Timeuuid(timeuuid) => Value::Text(timeuuid_to_uuid(*timeuuid)),
        CqlValue::Tuple(items) => Value::Array(
            items
                .iter()
                .map(|value| value.as_ref().map(value_from_cql).unwrap_or(Value::Null))
                .collect(),
        ),
        CqlValue::Uuid(uuid) => Value::Text(uuid.to_string()),
        CqlValue::Varint(varint) => match big_endian_to_i128(varint.as_signed_bytes_be_slice()) {
            // An integer column carries a number whenever the value fits one;
            // a varint past `i64` stays exact text rather than being rounded.
            Some(value) => match i64::try_from(value) {
                Ok(number) => Value::Int(number),
                Err(_) => Value::Decimal(value.to_string()),
            },
            None => Value::Unsupported("varint".to_string()),
        },
        // `CqlValue` is `#[non_exhaustive]`: a type a newer protocol adds is
        // reported as undecodable rather than guessed at.
        _ => Value::Unsupported("unsupported CQL value".to_string()),
    }
}

/// Converts a bound parameter, or refuses a type this part cannot bind.
///
/// The bind list holds `Option` so a `Null` parameter reaches the server as a
/// CQL null rather than an empty value.
pub(crate) fn values_to_cql(values: &[Value]) -> Result<Vec<Option<CqlValue>>, DbError> {
    values
        .iter()
        .map(|value| match value {
            Value::Null => Ok(None),
            Value::Bool(flag) => Ok(Some(CqlValue::Boolean(*flag))),
            Value::Int(number) => Ok(Some(match i32::try_from(*number) {
                Ok(narrow) => CqlValue::Int(narrow),
                Err(_) => CqlValue::BigInt(*number),
            })),
            Value::Float(number) => Ok(Some(CqlValue::Double(*number))),
            Value::Text(text) | Value::Json(text) => Ok(Some(CqlValue::Text(text.clone()))),
            Value::Bytes(bytes) => Ok(Some(CqlValue::Blob(bytes.clone()))),
            Value::DateTime(date_time) => Ok(Some(CqlValue::Timestamp(
                scylla::value::CqlTimestamp(date_time.timestamp_millis()),
            ))),
            Value::Date(date) => Ok(Some(CqlValue::Date(CqlDate::from(*date)))),
            Value::Time(time) => Ok(Some(CqlValue::Time(CqlTime(nanoseconds_since_midnight(
                *time,
            ))))),
            // The message stays free of the value: it reaches a toast and the
            // audit row.
            Value::Decimal(_) => Err(DbError::NotSupported(
                "Binding a decimal needs a prepared statement; that arrives in a later part"
                    .to_string(),
            )),
            Value::Array(_) | Value::Document(_) => Err(DbError::NotSupported(
                "Binding a collection or a user-defined type arrives in a later part".to_string(),
            )),
            Value::ObjectId(_) => Err(DbError::NotSupported(
                "Cassandra has no object id type".to_string(),
            )),
            Value::Unsupported(type_name) => Err(DbError::NotSupported(format!(
                "A value of type '{type_name}' cannot be bound"
            ))),
        })
        .collect()
}

fn timestamp_to_datetime(timestamp: scylla::value::CqlTimestamp) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(timestamp.0)
}

fn time_to_naive(time: CqlTime) -> Option<NaiveTime> {
    let nanos = u64::try_from(time.0).ok()?;
    let seconds = u32::try_from(nanos / 1_000_000_000).ok()?;
    NaiveTime::from_num_seconds_from_midnight_opt(seconds, (nanos % 1_000_000_000) as u32)
}

fn nanoseconds_since_midnight(time: NaiveTime) -> i64 {
    use chrono::Timelike;

    i64::from(time.num_seconds_from_midnight()) * 1_000_000_000 + i64::from(time.nanosecond())
}

fn timeuuid_to_uuid(timeuuid: CqlTimeuuid) -> String {
    uuid::Uuid::from(timeuuid).to_string()
}

fn format_duration(duration: CqlDuration) -> String {
    format!(
        "{}mo{}d{}ns",
        duration.months, duration.days, duration.nanoseconds
    )
}

/// Reads a big-endian two's-complement integer of at most 16 bytes.
fn big_endian_to_i128(bytes: &[u8]) -> Option<i128> {
    if bytes.len() > 16 {
        return None;
    }
    let mut value: i128 = if bytes.first().is_some_and(|byte| byte & 0x80 != 0) {
        -1
    } else {
        0
    };
    for byte in bytes {
        value = (value << 8) | i128::from(*byte);
    }
    Some(value)
}

/// Renders an unscaled integer and a decimal scale as an exact decimal string.
///
/// `None` when the scale is past what a string can usefully hold: a server
/// scale near `i32::MIN` would otherwise ask for gigabytes of zeros.
pub(crate) fn format_decimal(unscaled: i128, scale: i32) -> Option<String> {
    if scale.unsigned_abs() > MAX_DECIMAL_SCALE {
        return None;
    }
    let negative = unscaled < 0;
    let digits = unscaled.unsigned_abs().to_string();
    let body = if scale <= 0 {
        let zeros = usize::try_from(scale.unsigned_abs()).unwrap_or(usize::MAX);
        format!("{digits}{}", "0".repeat(zeros))
    } else {
        let scale = scale as usize;
        if digits.len() > scale {
            let (whole, fraction) = digits.split_at(digits.len() - scale);
            format!("{whole}.{fraction}")
        } else {
            format!("0.{}{digits}", "0".repeat(scale - digits.len()))
        }
    };
    Some(if negative { format!("-{body}") } else { body })
}
