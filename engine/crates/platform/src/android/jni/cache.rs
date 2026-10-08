use std::collections::HashMap;

use jni::objects::{GlobalRef, JClass, JStaticMethodID};

pub struct JavaMethodCache {
    class: GlobalRef,
    methods: HashMap<String, JStaticMethodID>,
}

impl JavaMethodCache {
    pub(crate) fn new(class: GlobalRef) -> Self {
        Self {
            class,
            methods: HashMap::new(),
        }
    }

    /// Returns the cached Java class as a `JClass<'static>`.
    ///
    /// The raw handle is valid as long as `self.class` is alive. No Drop is
    /// triggered through `JClass` because it carries no ownership of its own.
    #[inline]
    pub(crate) fn class(&self) -> JClass<'static> {
        // SAFETY: `self.class` is a JNI global reference; its raw handle is
        // always valid for the lifetime of `self`. `JClass` has no Drop, so
        // handing it out as `'static` cannot cause a double-free.
        unsafe { JClass::from_raw(self.class.as_obj().as_raw()) }
    }

    #[inline]
    pub(crate) fn get_method_id(&self, name: &str) -> Option<&JStaticMethodID> {
        self.methods.get(name)
    }

    #[inline]
    pub(crate) fn insert_method_id(&mut self, name: &str, method_id: JStaticMethodID) {
        self.methods.insert(name.to_string(), method_id);
    }
}
