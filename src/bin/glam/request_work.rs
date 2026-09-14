//! Buildable bridge while executable-owned effect handlers migrate to
//! pollable request work in W5C.5c.

use glam::Value;
use glam::reflection::{
    ReflectionHost, ReflectionRequest, ReflectionRequestWork, ReflectionTransaction,
    RequestContext, SpecializationRequestInput, SpecializationRequestPoll,
    SpecializationRequestWork, TaskHalt, TaskSpecialization,
};

pub(crate) struct SynchronousRequestWork<S: TaskSpecialization> {
    request: Option<S::Request>,
    arguments: Option<Vec<Value>>,
}

impl<S: TaskSpecialization> SynchronousRequestWork<S> {
    pub(crate) fn new(request: S::Request, arguments: Vec<Value>) -> Self {
        Self {
            request: Some(request),
            arguments: Some(arguments),
        }
    }
}

pub(crate) trait SynchronousTaskSpecialization: TaskSpecialization {
    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<Value>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<glam::reflection::RequestResult, TaskHalt>;
}

impl<S> SpecializationRequestWork<S> for SynchronousRequestWork<S>
where
    S: SynchronousTaskSpecialization,
{
    fn poll(
        &mut self,
        specialization: &S,
        input: Option<SpecializationRequestInput>,
        context: &mut RequestContext<'_, S>,
    ) -> Result<SpecializationRequestPoll, TaskHalt> {
        assert!(
            input.is_none(),
            "synchronous request work cannot receive a demand completion"
        );
        let request = self
            .request
            .as_ref()
            .expect("synchronous request work must retain its request")
            .clone();
        let arguments = self
            .arguments
            .as_ref()
            .expect("synchronous request work must retain its arguments")
            .clone();
        let result = specialization.handle_request(request, arguments, context)?;
        self.request = None;
        self.arguments = None;
        Ok(SpecializationRequestPoll::Complete(result))
    }
}

pub(crate) enum ReflectionOrSynchronousRequestWork<S: TaskSpecialization> {
    Reflection(ReflectionRequestWork),
    Synchronous(SynchronousRequestWork<S>),
}

impl<S: TaskSpecialization> ReflectionOrSynchronousRequestWork<S> {
    pub(crate) fn reflection(request: ReflectionRequest, arguments: Vec<Value>) -> Self {
        Self::Reflection(ReflectionRequestWork::new(request, arguments))
    }

    pub(crate) fn synchronous(request: S::Request, arguments: Vec<Value>) -> Self {
        Self::Synchronous(SynchronousRequestWork::new(request, arguments))
    }
}

impl<S> SpecializationRequestWork<S> for ReflectionOrSynchronousRequestWork<S>
where
    S: SynchronousTaskSpecialization,
    S::Host: ReflectionHost<S>,
    S::Journal: ReflectionTransaction,
{
    fn poll(
        &mut self,
        specialization: &S,
        input: Option<SpecializationRequestInput>,
        context: &mut RequestContext<'_, S>,
    ) -> Result<SpecializationRequestPoll, TaskHalt> {
        match self {
            Self::Reflection(work) => work.poll(specialization, input, context),
            Self::Synchronous(work) => work.poll(specialization, input, context),
        }
    }
}
