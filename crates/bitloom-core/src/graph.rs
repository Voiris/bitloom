use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use daggy::stable_dag::{NodeIndex, StableDag};
use thiserror::Error;

use crate::node::ErasedNodeRef;
use crate::{EffectKind, Task, TaskInstanceId, TaskKind, TaskRef, TaskSnapshot};

#[derive(Debug, Error)]
pub enum GraphBuildError {
    #[error("at least one root task is required")]
    NoRoots,
    #[error("task {task_id} has an empty task kind")]
    EmptyTaskKind { task_id: TaskInstanceId },
    #[error("task kind `{kind}` is used by both `{first_type}` and `{second_type}`")]
    TaskKindCollision { kind: TaskKind, first_type: &'static str, second_type: &'static str },
    #[error("cycle detected: {path}")]
    Cycle { path: String },
    #[error(
        "tasks {first_task} and {second_task} both claim output path {}",
        path.display()
    )]
    OutputConflict { path: PathBuf, first_task: TaskInstanceId, second_task: TaskInstanceId },
    #[error("failed to add graph edge: {message}")]
    Edge { message: String },
    #[error("irreversible task {task_id} must be terminal but has dependent tasks")]
    IrreversibleHasDependents { task_id: TaskInstanceId },
}

#[derive(Default)]
pub struct GraphBuilder {
    roots: Vec<ErasedNodeRef>,
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_root<T: Task>(&mut self, root: &TaskRef<T>) -> &mut Self {
        if !self.roots.iter().any(|candidate| candidate.id() == root.id()) {
            self.roots.push(root.erased());
        }
        self
    }

    pub fn with_root<T: Task>(mut self, root: &TaskRef<T>) -> Self {
        self.add_root(root);
        self
    }

    pub fn build(self) -> Result<TaskGraph, GraphBuildError> {
        if self.roots.is_empty() {
            return Err(GraphBuildError::NoRoots);
        }

        let mut state = BuildState::default();
        for root in self.roots {
            state.visit(root)?;
        }
        state.validate_irreversible_tasks()?;

        Ok(TaskGraph {
            dag: state.dag,
            nodes: state.nodes,
            children: state.children,
            parents: state.parents,
            execution_started: AtomicBool::new(false),
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Mark {
    Visiting,
    Done(usize),
}

#[derive(Default)]
struct BuildState {
    dag: StableDag<ErasedNodeRef, ()>,
    nodes: Vec<ErasedNodeRef>,
    children: Vec<Vec<usize>>,
    parents: Vec<Vec<usize>>,
    marks: HashMap<TaskInstanceId, Mark>,
    stack: Vec<TaskInstanceId>,
    labels: HashMap<TaskInstanceId, Arc<str>>,
    task_kinds: HashMap<TaskKind, &'static str>,
    output_claims: HashMap<PathBuf, TaskInstanceId>,
}

impl BuildState {
    fn visit(&mut self, node: ErasedNodeRef) -> Result<usize, GraphBuildError> {
        match self.marks.get(&node.id()).copied() {
            Some(Mark::Done(index)) => return Ok(index),
            Some(Mark::Visiting) => return Err(self.cycle_error(node.id())),
            None => {}
        }

        if node.kind().as_str().trim().is_empty() {
            return Err(GraphBuildError::EmptyTaskKind { task_id: node.id() });
        }

        if let Some(existing) = self.task_kinds.insert(node.kind(), node.rust_type_name())
            && existing != node.rust_type_name()
        {
            return Err(GraphBuildError::TaskKindCollision {
                kind: node.kind(),
                first_type: existing,
                second_type: node.rust_type_name(),
            });
        }

        self.labels.insert(node.id(), node.label());
        self.marks.insert(node.id(), Mark::Visiting);
        self.stack.push(node.id());

        let dependencies = node.dependencies();
        let mut dependency_indices = Vec::with_capacity(dependencies.len());
        for dependency in dependencies {
            dependency_indices.push(self.visit(dependency)?);
        }

        self.stack.pop();

        for path in node.output_claims() {
            if let Some(first_task) = self.output_claims.insert(path.clone(), node.id())
                && first_task != node.id()
            {
                return Err(GraphBuildError::OutputConflict {
                    path,
                    first_task,
                    second_task: node.id(),
                });
            }
        }

        let index = self.nodes.len();
        let dag_index = self.dag.add_node(node.clone());
        debug_assert_eq!(dag_index.index(), index);
        self.nodes.push(node);
        self.children.push(Vec::new());
        self.parents.push(Vec::new());

        for dependency_index in dependency_indices {
            self.dag
                .add_edge(NodeIndex::new(dependency_index), NodeIndex::new(index), ())
                .map_err(|error| GraphBuildError::Edge { message: error.to_string() })?;
            self.children[dependency_index].push(index);
            self.parents[index].push(dependency_index);
        }

        self.marks.insert(self.nodes[index].id(), Mark::Done(index));
        Ok(index)
    }

    fn cycle_error(&self, repeated: TaskInstanceId) -> GraphBuildError {
        let start = self.stack.iter().position(|id| *id == repeated).unwrap_or(0);
        let mut cycle = self.stack[start..]
            .iter()
            .map(|id| self.labels.get(id).map_or_else(|| id.to_string(), ToString::to_string))
            .collect::<Vec<_>>();
        cycle.push(
            self.labels.get(&repeated).map_or_else(|| repeated.to_string(), ToString::to_string),
        );
        GraphBuildError::Cycle { path: cycle.join(" -> ") }
    }

    fn validate_irreversible_tasks(&self) -> Result<(), GraphBuildError> {
        for (index, node) in self.nodes.iter().enumerate() {
            if node.effect() == EffectKind::Irreversible && !self.children[index].is_empty() {
                return Err(GraphBuildError::IrreversibleHasDependents { task_id: node.id() });
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct TaskGraph {
    #[allow(dead_code)]
    dag: StableDag<ErasedNodeRef, ()>,
    pub(crate) nodes: Vec<ErasedNodeRef>,
    pub(crate) children: Vec<Vec<usize>>,
    pub(crate) parents: Vec<Vec<usize>>,
    execution_started: AtomicBool,
}

impl TaskGraph {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn monitor(&self) -> TaskMonitor {
        TaskMonitor { nodes: self.nodes.clone() }
    }

    pub(crate) fn begin_execution(&self) -> bool {
        self.execution_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

#[derive(Clone)]
pub struct TaskMonitor {
    nodes: Vec<ErasedNodeRef>,
}

impl TaskMonitor {
    pub fn snapshots(&self) -> Vec<TaskSnapshot> {
        self.nodes.iter().map(ErasedNodeRef::snapshot).collect()
    }

    pub fn is_finished(&self) -> bool {
        self.nodes.iter().all(|node| node.snapshot().phase.is_terminal())
    }
}
