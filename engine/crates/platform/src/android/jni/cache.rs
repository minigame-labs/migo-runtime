use std::collections::HashMap;

use jni::objects::{Global, JClass, JStaticMethodID};

pub struct JavaMethodCache {
    class: Global<JClass<'static>>,
    methods: HashMap<String, JStaticMethodID>,
}

impl JavaMethodCache {
    pub(crate) fn new(class: Global<JClass<'static>>) -> Self {
        Self {
            class,
            methods: HashMap::new(),
        }
    }

    /// Borrow the class through its owning global reference.
    #[inline]
    pub(crate) fn class(&self) -> &JClass<'static> {
        &self.class
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
