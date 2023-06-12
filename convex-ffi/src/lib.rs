use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use convex::{ConvexClient, FunctionResult, Value as ConvexValue};
use futures::{
    channel::oneshot::{self, Sender},
    pin_mut, select_biased, FutureExt, StreamExt,
};
use ordered_float::OrderedFloat;
use tokio::sync::{MappedMutexGuard, Mutex, MutexGuard};
use uniffi::{
    check_remaining,
    deps::bytes::{Buf, BufMut},
    ffi_converter_default_return, ffi_converter_rust_buffer_lift_and_lower, metadata, FfiConverter,
    MetadataBuffer,
};

uniffi::include_scaffolding!("lib");

#[derive(uniffi::Object)]
pub struct Client {
    deployment_url: String,
    _inner: Mutex<Option<ConvexClient>>,
}

type Float64 = OrderedFloat<f64>;

impl UniffiCustomTypeConverter for Float64 {
    type Builtin = f64;

    fn into_custom(val: Self::Builtin) -> uniffi::Result<Self> {
        Ok(Self(val))
    }

    fn from_custom(obj: Self) -> Self::Builtin {
        obj.0
    }
}

// `BTreeMap` and `BTreeSet` use the same serialization representation as `HashMap` and `Vec`, that's why the `TYPE_ID_META` is `HashMap` and `Vec`
// This is only possible with the experimental proc-macro feature to describe the interface.
// Unsafe implementations are the same than the ones in `uniffi` crate for `HashMap` and `Vec`

type UT = crate::UniFfiTag;

type ValueObject = BTreeMap<String, Value>;

unsafe impl FfiConverter<UT> for ValueObject {
    ffi_converter_rust_buffer_lift_and_lower!(UT);
    ffi_converter_default_return!(UT);

    fn write(obj: ValueObject, buf: &mut Vec<u8>) {
        // TODO: would be nice not to panic here :-/
        let len = i32::try_from(obj.len()).unwrap();
        buf.put_i32(len);
        for (key, value) in obj {
            <String as FfiConverter<UT>>::write(key, buf);
            <Value as FfiConverter<UT>>::write(value, buf);
        }
    }

    fn try_read(buf: &mut &[u8]) -> anyhow::Result<ValueObject> {
        check_remaining(buf, 4)?;
        let len = usize::try_from(buf.get_i32())?;
        let mut map = BTreeMap::new();
        for _ in 0..len {
            let key = <String as FfiConverter<UT>>::try_read(buf)?;
            let value = <Value as FfiConverter<UT>>::try_read(buf)?;
            map.insert(key, value);
        }
        Ok(map)
    }

    const TYPE_ID_META: MetadataBuffer = MetadataBuffer::from_code(metadata::codes::TYPE_HASH_MAP)
        .concat(<String as FfiConverter<UT>>::TYPE_ID_META)
        .concat(<Value as FfiConverter<UT>>::TYPE_ID_META);
}

type ValueMap = BTreeMap<Value, Value>;

unsafe impl FfiConverter<UT> for ValueMap {
    ffi_converter_rust_buffer_lift_and_lower!(UT);
    ffi_converter_default_return!(UT);

    fn write(obj: ValueMap, buf: &mut Vec<u8>) {
        // TODO: would be nice not to panic here :-/
        let len = i32::try_from(obj.len()).unwrap();
        buf.put_i32(len);
        for (key, value) in obj {
            <Value as FfiConverter<UT>>::write(key, buf);
            <Value as FfiConverter<UT>>::write(value, buf);
        }
    }

    fn try_read(buf: &mut &[u8]) -> anyhow::Result<ValueMap> {
        check_remaining(buf, 4)?;
        let len = usize::try_from(buf.get_i32())?;
        let mut map = BTreeMap::new();
        for _ in 0..len {
            let key = <Value as FfiConverter<UT>>::try_read(buf)?;
            let value = <Value as FfiConverter<UT>>::try_read(buf)?;
            map.insert(key, value);
        }
        Ok(map)
    }

    const TYPE_ID_META: MetadataBuffer = MetadataBuffer::from_code(metadata::codes::TYPE_HASH_MAP)
        .concat(<Value as FfiConverter<UT>>::TYPE_ID_META)
        .concat(<Value as FfiConverter<UT>>::TYPE_ID_META);
}

type ValueSet = BTreeSet<Value>;

unsafe impl FfiConverter<UT> for ValueSet {
    ffi_converter_rust_buffer_lift_and_lower!(UT);
    ffi_converter_default_return!(UT);

    fn write(set: ValueSet, buf: &mut Vec<u8>) {
        // TODO: would be nice not to panic here :-/
        let len = i32::try_from(set.len()).unwrap();
        buf.put_i32(len);
        for value in set {
            <Value as FfiConverter<UT>>::write(value, buf);
        }
    }

    fn try_read(buf: &mut &[u8]) -> anyhow::Result<ValueSet> {
        check_remaining(buf, 4)?;
        let len = usize::try_from(buf.get_i32())?;
        let mut map = BTreeSet::new();
        for _ in 0..len {
            let value = <Value as FfiConverter<UT>>::try_read(buf)?;
            map.insert(value);
        }
        Ok(map)
    }

    const TYPE_ID_META: MetadataBuffer = MetadataBuffer::from_code(metadata::codes::TYPE_VEC)
        .concat(<Value as FfiConverter<UT>>::TYPE_ID_META);
}

#[uniffi::export(callback_interface)]
pub trait Callback: Send + Sync {
    fn update(&self, value: Value);
}

#[derive(uniffi::Error)]
pub enum SubscribeError {
    Generic { message: String },
}

#[uniffi::export]
fn set_tracing_subscriber() {
    tracing_subscriber::fmt::init();
}

#[uniffi::export]
impl Client {
    #[uniffi::constructor]
    fn new(deployment_url: String) -> Arc<Self> {
        Arc::new(Self {
            deployment_url,
            _inner: Mutex::new(None),
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Client {
    async fn connect(&self) {
        let mut inner = self._inner.lock().await;

        let client = ConvexClient::new(&self.deployment_url).await;
        if let Ok(client) = client {
            inner.replace(client);
        }
    }

    async fn close(&self) {
        self._inner.lock().await.take();
    }

    pub async fn subscribe(
        &self,
        path: String,
        args: ValueObject,
        callback: Box<dyn Callback>,
    ) -> Result<Arc<Subscription>, SubscribeError> {
        let mut client = self.client().await?;
        let args = to_convex_args(args);

        let mut subscription = client.subscribe(&path, args).await.unwrap();

        let (sender, receiver) = oneshot::channel::<()>();

        let unsubscribe_fut = receiver.fuse();

        tokio::spawn(async move {
            pin_mut!(unsubscribe_fut);
            loop {
                select_biased! {
                    result = subscription.next().fuse() => {
                        if let Some(result) = result {
                            match result {
                                FunctionResult::Value(value) => {
                                    callback.update(value.into());
                                }
                                FunctionResult::ErrorMessage(message) => {
                                    tracing::error!("Subscription error: {}", message);
                                }
                            }
                        }
                    },
                    _ = unsubscribe_fut => {
                        break
                    }
                }
            }
        });

        Ok(Arc::new(Subscription {
            sender: std::sync::Mutex::new(Some(sender)),
        }))
    }

    pub async fn query(&self, path: String, args: ValueObject) -> Result<Value, SubscribeError> {
        let mut client = self.client().await?;
        let args = to_convex_args(args);

        let result = client.query(&path, args).await.unwrap();
        match result {
            FunctionResult::Value(value) => Ok(value.into()),
            FunctionResult::ErrorMessage(message) => Err(SubscribeError::Generic { message }),
        }
    }

    pub async fn mutation(&self, path: String, args: ValueObject) -> Result<Value, SubscribeError> {
        let mut client = self.client().await?;
        let args = to_convex_args(args);

        let result = client.mutation(&path, args).await.unwrap();
        match result {
            FunctionResult::Value(value) => Ok(value.into()),
            FunctionResult::ErrorMessage(message) => Err(SubscribeError::Generic { message }),
        }
    }

    pub async fn action(&self, path: String, args: ValueObject) -> Result<Value, SubscribeError> {
        let mut client = self.client().await?;
        let args = to_convex_args(args);

        let result = client.action(&path, args).await.unwrap();
        match result {
            FunctionResult::Value(value) => Ok(value.into()),
            FunctionResult::ErrorMessage(message) => Err(SubscribeError::Generic { message }),
        }
    }
}

impl Client {
    async fn client(&self) -> Result<MappedMutexGuard<ConvexClient>, SubscribeError> {
        let lock = self._inner.lock().await;
        if lock.is_some() {
            Ok(MutexGuard::map(lock, |lock| lock.as_mut().unwrap()))
        } else {
            Err(SubscribeError::Generic {
                message: "No client set".to_string(),
            })
        }
    }
}

#[derive(uniffi::Object)]
pub struct Subscription {
    sender: std::sync::Mutex<Option<Sender<()>>>,
}

#[uniffi::export]
impl Subscription {
    fn cancel(&self) {
        if let Ok(sender) = self.sender.lock().as_mut() {
            if let Some(sender) = sender.take() {
                let _ = sender.send(());
            }
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        tracing::debug!("Subscription dropped!");
        self.cancel();
    }
}

fn to_convex_args(args: ValueObject) -> BTreeMap<String, ConvexValue> {
    BTreeMap::from_iter(args.into_iter().map(|(key, value)| (key, value.into())))
}

// A compatible uniffi built-in types Convex Value
#[derive(uniffi::Enum, PartialOrd, Ord, PartialEq, Eq, Clone, Debug)]
pub enum Value {
    Id { id: String },
    Null,
    Int { value: i64 },
    Float { value: Float64 },
    Bool { value: bool },
    String { value: String },
    Bytes { value: Vec<u8> },
    Array { value: Vec<Value> },
    Set { value: ValueSet },
    Map { value: ValueMap },
    Object { value: ValueObject },
}

impl From<ConvexValue> for Value {
    fn from(value: ConvexValue) -> Self {
        match value {
            ConvexValue::Id(id) => Value::Id { id: id.to_string() },
            ConvexValue::Null => Value::Null,
            ConvexValue::Int64(value) => Value::Int { value },
            ConvexValue::Float64(value) => Value::Float {
                value: OrderedFloat(value),
            },
            ConvexValue::Boolean(value) => Value::Bool { value },
            ConvexValue::String(value) => Value::String { value },
            ConvexValue::Bytes(value) => Value::Bytes { value },
            ConvexValue::Array(value) => Value::Array {
                value: value.into_iter().map(|v| v.into()).collect(),
            },
            ConvexValue::Set(set) => Value::Set {
                value: set.into_iter().map(|value| value.into()).collect(),
            },
            ConvexValue::Map(map) => Value::Map {
                value: map
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect(),
            },
            ConvexValue::Object(object) => Value::Object {
                value: object
                    .into_iter()
                    .map(|(key, value)| (key, value.into()))
                    .collect(),
            },
        }
    }
}

impl From<Value> for ConvexValue {
    fn from(value: Value) -> Self {
        match value {
            Value::Id { id } => ConvexValue::Id(id.into()),
            Value::Null => ConvexValue::Null,
            Value::Int { value } => ConvexValue::Int64(value),
            Value::Float { value } => ConvexValue::Float64(value.0),
            Value::Bool { value } => ConvexValue::Boolean(value),
            Value::String { value } => ConvexValue::String(value),
            Value::Bytes { value } => ConvexValue::Bytes(value),
            Value::Array { value } => {
                ConvexValue::Array(value.into_iter().map(|v| v.into()).collect())
            }
            Value::Set { value } => {
                ConvexValue::Set(value.into_iter().map(|value| value.into()).collect())
            }
            Value::Map { value } => ConvexValue::Map(
                value
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect(),
            ),
            Value::Object { value } => ConvexValue::Object(
                value
                    .into_iter()
                    .map(|(key, value)| (key, value.into()))
                    .collect(),
            ),
        }
    }
}
