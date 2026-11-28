//! A model provider across C: asked through a call that returns at once, answered through a
//! completion the extension keeps and fulfils from wherever it likes.
//!
//! This is the whole of asynchrony at this boundary. There is no runtime on either side and
//! none is shared: the future the loop awaits holds a waker, and completing wakes it.

use core::pin::Pin;
use core::task::{Context as Task, Poll, Waker};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use corduroy_harness::provider::{Provider, ProviderError, provider as provided};
use corduroy_harness::{ModelReply, ModelRequest};

use super::{User, applying, copy, named, sized};
use crate::abi::{self, Str};

mod wire;

/// An answer the extension owes: what a `corduroy_completion` points to.
#[derive(Default)]
struct Owed {
    slot: Mutex<Slot>,
}

#[derive(Default)]
struct Slot {
    answer: Option<(i32, String)>,
    waker: Option<Waker>,
}

/// The loop's side of an [`Owed`].
struct Answer(Arc<Owed>);

impl Future for Answer {
    type Output = (i32, String);

    fn poll(self: Pin<&mut Self>, cx: &mut Task<'_>) -> Poll<Self::Output> {
        let mut slot = self.0.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(answer) = slot.answer.take() {
            return Poll::Ready(answer);
        }
        slot.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

pub(super) extern "C" fn complete(done: *mut abi::Completion, status: i32, text: Str) {
    if done.is_null() {
        return;
    }
    // SAFETY: a completion is the pointer `CProvider::start` made with `Arc::into_raw`, and
    // the header has the extension complete each exactly once.
    let owed = unsafe { Arc::from_raw(done.cast::<Owed>().cast_const()) };
    // SAFETY: rule 1.
    let answer = unsafe { copy(text) }.map_or_else(
        || (abi::FAILED, "the answer was not UTF-8".to_owned()),
        |text| (status, text),
    );
    let waker = {
        let mut slot = owed.slot.lock().unwrap_or_else(PoisonError::into_inner);
        slot.answer = Some(answer);
        slot.waker.take()
    };
    if let Some(waker) = waker {
        waker.wake();
    }
}

struct CProvider {
    user: User,
    complete: abi::CompleteFn,
}

impl CProvider {
    /// Hand `request` to the extension and return what it now owes.
    fn start(&self, request: &str) -> Answer {
        let owed = Arc::new(Owed::default());
        let done = Arc::into_raw(Arc::clone(&owed))
            .cast_mut()
            .cast::<abi::Completion>();
        // SAFETY: the function and pointer the extension registered together; the request
        // outlives the call, and `done` is the extension's until it completes it.
        unsafe { (self.complete)(self.user.get(), Str::of(request), done) };
        Answer(owed)
    }
}

#[async_trait]
impl Provider for CProvider {
    async fn complete(&self, request: &ModelRequest) -> Result<ModelReply, ProviderError> {
        match self.start(&wire::request(request)).await {
            (abi::OK, reply) => wire::reply(&reply).map_err(ProviderError::new),
            (_, why) => Err(ProviderError::new(why)),
        }
    }
}

pub(super) extern "C" fn provider(
    ctx: *mut abi::Ctx,
    name: Str,
    provider: *const abi::Provider,
) -> i32 {
    applying(ctx, |applying| {
        // SAFETY: rule 1.
        let name = unsafe { named(name) }?;
        // SAFETY: the extension passes a `corduroy_provider` it filled in.
        let table = unsafe { sized(provider) }?;
        let complete = table.complete.ok_or("a provider has no function")?;
        let entry = CProvider {
            user: User(table.user),
            complete,
        };
        // A child plugin: it goes when the extension does.
        let _child = applying.ctx.plugin(provided(name, Arc::new(entry)));
        Ok(())
    })
}
