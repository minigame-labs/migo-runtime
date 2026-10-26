#![allow(non_snake_case)]

mod cache;
mod env;
mod inbound;
pub(crate) mod outbound;
mod registration;
mod safe;
mod tls;
mod utils;

pub use env::{init_jni_env, with_env};
pub use inbound::on_load;
pub use outbound::*;

pub(crate) use inbound::*;
pub(crate) use registration::{register_java_exports, register_native_exports};

pub(crate) use cache::JavaMethodCache;
pub(crate) use env::JAVA_METHOD_CACHE;

pub(crate) use safe::jni_safe;
pub(crate) use utils::*;
