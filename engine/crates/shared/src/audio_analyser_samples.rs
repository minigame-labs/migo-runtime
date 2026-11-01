//! An analyser snapshot keeps its storage owner through transport and serialization.
use serde::{Serialize, Serializer};
use std::ops::{Deref, DerefMut};

pub trait AnalyserStorage<T>: Send {
    fn samples(&self) -> &[T];
    fn samples_mut(&mut self) -> &mut [T];
}
impl<T: Send> AnalyserStorage<T> for Vec<T> {
    fn samples(&self) -> &[T] {
        self
    }
    fn samples_mut(&mut self) -> &mut [T] {
        self
    }
}

pub struct AnalyserSamples<T>(Box<dyn AnalyserStorage<T>>);
impl<T> AnalyserSamples<T> {
    pub fn new(storage: impl AnalyserStorage<T> + 'static) -> Self {
        Self(Box::new(storage))
    }
}
impl<T: Send + 'static> From<Vec<T>> for AnalyserSamples<T> {
    fn from(samples: Vec<T>) -> Self {
        Self::new(samples)
    }
}
impl<T> AsRef<[T]> for AnalyserSamples<T> {
    fn as_ref(&self) -> &[T] {
        self.0.samples()
    }
}
impl<T> AsMut<[T]> for AnalyserSamples<T> {
    fn as_mut(&mut self) -> &mut [T] {
        self.0.samples_mut()
    }
}
impl<T> Deref for AnalyserSamples<T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}
impl<T> DerefMut for AnalyserSamples<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut()
    }
}
impl<T: std::fmt::Debug> std::fmt::Debug for AnalyserSamples<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_ref().fmt(f)
    }
}
impl<T: Serialize> Serialize for AnalyserSamples<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_ref().serialize(serializer)
    }
}
