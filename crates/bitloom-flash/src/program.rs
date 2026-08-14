use std::borrow::Cow;
use std::error::Error;
use std::future::Future;
use std::sync::Arc;

use bitloom_core::{EffectKind, Task, TaskContext, TaskKind, TaskRef};

use crate::ProgramImage;

pub trait ProgrammerBackend: Send + Sync + 'static {
    type Error: Error + Send + Sync + 'static;

    fn program(
        &self,
        context: TaskContext,
        image: Arc<ProgramImage>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

pub struct ProgramTask<D, B>
where
    D: Task<Output = ProgramImage>,
    B: ProgrammerBackend,
{
    image: TaskRef<D>,
    backend: B,
}

impl<D, B> ProgramTask<D, B>
where
    D: Task<Output = ProgramImage>,
    B: ProgrammerBackend,
{
    pub fn new(image: TaskRef<D>, backend: B) -> Self {
        Self { image, backend }
    }
}

impl<D, B> Task for ProgramTask<D, B>
where
    D: Task<Output = ProgramImage>,
    B: ProgrammerBackend,
{
    type Output = ();
    type Error = B::Error;
    type Dependencies = TaskRef<D>;

    const KIND: TaskKind = TaskKind::new("bitloom.flash.program");

    fn label(&self) -> Cow<'static, str> {
        "Program FPGA".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.image
    }

    fn effect(&self) -> EffectKind {
        EffectKind::Irreversible
    }

    async fn run(
        &self,
        context: TaskContext,
        image: Arc<ProgramImage>,
    ) -> Result<Self::Output, Self::Error> {
        self.backend.program(context, image).await
    }
}
