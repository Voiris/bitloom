use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use crate::node::ErasedNodeRef;
use crate::{Task, TaskFailure, TaskInstanceId, TaskRef};

/// A typed description of all nodes that must finish before a task can run.
pub trait Dependencies: Send + Sync + 'static {
    type Resolved: Send + 'static;

    fn collect(&self, collector: &mut DependencyCollector);

    fn resolve(&self) -> impl Future<Output = Result<Self::Resolved, Arc<TaskFailure>>> + Send;
}

#[derive(Default)]
pub struct DependencyCollector {
    nodes: Vec<ErasedNodeRef>,
    ids: HashSet<TaskInstanceId>,
}

impl DependencyCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add<T: Task>(&mut self, dependency: &TaskRef<T>) {
        if self.ids.insert(dependency.id()) {
            self.nodes.push(dependency.erased());
        }
    }

    pub(crate) fn into_nodes(self) -> Vec<ErasedNodeRef> {
        self.nodes
    }
}

impl Dependencies for () {
    type Resolved = ();

    fn collect(&self, _collector: &mut DependencyCollector) {}

    async fn resolve(&self) -> Result<Self::Resolved, Arc<TaskFailure>> {
        Ok(())
    }
}

impl<T: Task> Dependencies for TaskRef<T> {
    type Resolved = Arc<T::Output>;

    fn collect(&self, collector: &mut DependencyCollector) {
        collector.add(self);
    }

    async fn resolve(&self) -> Result<Self::Resolved, Arc<TaskFailure>> {
        self.result().await
    }
}

impl<D: Dependencies> Dependencies for Option<D> {
    type Resolved = Option<D::Resolved>;

    fn collect(&self, collector: &mut DependencyCollector) {
        if let Some(dependency) = self {
            dependency.collect(collector);
        }
    }

    async fn resolve(&self) -> Result<Self::Resolved, Arc<TaskFailure>> {
        match self {
            Some(dependency) => dependency.resolve().await.map(Some),
            None => Ok(None),
        }
    }
}

impl<D: Dependencies> Dependencies for Vec<D> {
    type Resolved = Vec<D::Resolved>;

    fn collect(&self, collector: &mut DependencyCollector) {
        for dependency in self {
            dependency.collect(collector);
        }
    }

    async fn resolve(&self) -> Result<Self::Resolved, Arc<TaskFailure>> {
        let mut resolved = Vec::with_capacity(self.len());
        for dependency in self {
            resolved.push(dependency.resolve().await?);
        }
        Ok(resolved)
    }
}

macro_rules! tuple_dependencies {
    ($($type:ident.$index:tt => $value:ident),+ $(,)?) => {
        impl<$($type: Dependencies),+> Dependencies for ($($type,)+) {
            type Resolved = ($($type::Resolved,)+);

            fn collect(&self, collector: &mut DependencyCollector) {
                $(self.$index.collect(collector);)+
            }

            async fn resolve(&self) -> Result<Self::Resolved, Arc<TaskFailure>> {
                $(let $value = self.$index.resolve().await?;)+
                Ok(($($value,)+))
            }
        }
    };
}

tuple_dependencies!(A.0 => a, B.1 => b);
tuple_dependencies!(A.0 => a, B.1 => b, C.2 => c);
tuple_dependencies!(A.0 => a, B.1 => b, C.2 => c, D.3 => d);
